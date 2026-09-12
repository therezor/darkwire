//! The serde and JSON Schema plumbing every mirrored type shares.
//!
//! Nothing here is a wire shape. It is the small set of building blocks that
//! let a plain `derive` reproduce what the browser's schemas express with
//! combinators: a string literal that is a type, a nullable-but-required
//! field, a number that arrives as a string, a positive integer, and the
//! transforms that make the generated schema say what the browser's does.

use std::borrow::Cow;
use std::fmt;
use std::marker::PhantomData;

use indexmap::IndexMap;
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::de::{self, Deserializer, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The largest integer JavaScript represents exactly.
///
/// Every integer on the wire carries this ceiling because the other end of
/// the wire is a browser: a value past it would round in transit and the two
/// sides would disagree about a number neither of them could see was wrong.
pub const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// An object whose keys are strings and whose values are opaque JSON.
pub type LooseObject = IndexMap<String, Value>;

/// A JSON object as a map, insertion order kept so a file re-serialises the
/// way it was written.
pub type Object = IndexMap<String, Value>;

/// A field the schema marks `default: {}`: the empty object is fed through the
/// child type so its own defaults apply, rather than restating every leaf.
pub fn prefault(schema: &mut Schema) {
    schema.insert("default".into(), Value::Object(serde_json::Map::new()));
}

/// A positive integer, spelled `exclusiveMinimum: 0` the way the source does.
///
/// Applied beside a `range(min = 1, ...)` validation attribute, which is what
/// enforces the bound; this only changes how the bound is written.
pub fn positive(schema: &mut Schema) {
    if let Some(object) = schema.as_object_mut() {
        object.remove("minimum");
        object.insert("exclusiveMinimum".into(), Value::from(0));
    }
}

/// Marks the schema of a required field that may be `null`.
///
/// The field's Rust type stays `Option<T>`; this only decides what the
/// published schema says about it. An `Option` on its own describes a field a
/// client may omit, and the generator strips the `null` serde would also
/// accept for it (see the schema generator in the registry). A field that is
/// deliberately nullable — `null` meaning "delete this" or "no such thing" —
/// names this type instead, and `oneOf` is what tells the two apart.
pub struct Nullable<T>(PhantomData<T>);

impl<T: JsonSchema> JsonSchema for Nullable<T> {
    fn schema_name() -> Cow<'static, str> {
        format!("Nullable_{}", T::schema_name()).into()
    }

    fn inline_schema() -> bool {
        true
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let inner = generator.subschema_for::<T>();
        json_schema!({ "oneOf": [inner, { "type": "null" }] })
    }
}

/// A string literal as a type: it serialises to its one value and refuses
/// anything else, which is what makes a discriminated union's `type` field a
/// real field on every variant rather than a tag bolted on from outside.
macro_rules! literal {
    ($(#[$meta:meta])* $vis:vis struct $name:ident = $value:literal;) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
        $vis struct $name;

        impl $name {
            /// The one string this type stands for.
            pub const VALUE: &'static str = $value;
        }

        impl ::serde::Serialize for $name {
            fn serialize<S: ::serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str($value)
            }
        }

        impl<'de> ::serde::Deserialize<'de> for $name {
            fn deserialize<D: ::serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let text = <::std::borrow::Cow<'de, str>>::deserialize(d)?;
                if text == $value {
                    Ok(Self)
                } else {
                    Err(::serde::de::Error::invalid_value(
                        ::serde::de::Unexpected::Str(&text),
                        &concat!("the literal \"", $value, "\""),
                    ))
                }
            }
        }

        impl ::schemars::JsonSchema for $name {
            fn schema_name() -> ::std::borrow::Cow<'static, str> {
                stringify!($name).into()
            }

            fn inline_schema() -> bool {
                true
            }

            fn json_schema(_: &mut ::schemars::SchemaGenerator) -> ::schemars::Schema {
                ::schemars::json_schema!({ "type": "string", "const": $value })
            }
        }
    };
}
pub(crate) use literal;

/// A union discriminated on one string field.
///
/// Every variant is a struct that carries the discriminator as a literal
/// field of its own, so each stands alone as a schema and the union adds
/// nothing but the choice. Deserialisation reads the discriminator first and
/// hands the whole object to the one variant it names, so a malformed frame
/// is reported against that variant rather than as "matched nothing".
macro_rules! tagged_union {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident by $tag:literal {
            $($(#[$vmeta:meta])* $variant:ident($ty:ty) = $value:literal,)+
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, ::serde::Serialize, ::schemars::JsonSchema, ::garde::Validate)]
        #[serde(untagged)]
        $vis enum $name {
            $($(#[$vmeta])* $variant(#[garde(dive)] $ty),)+
        }

        impl $name {
            /// The discriminator field every variant carries.
            pub const TAG: &'static str = $tag;

            /// Every discriminator value, in declaration order.
            pub const VALUES: &'static [&'static str] = &[$($value),+];

            /// The discriminator value of this variant.
            pub fn tag(&self) -> &'static str {
                match self {
                    $(Self::$variant(_) => $value,)+
                }
            }
        }

        $(impl From<$ty> for $name {
            fn from(value: $ty) -> Self {
                Self::$variant(value)
            }
        })+

        impl<'de> ::serde::Deserialize<'de> for $name {
            fn deserialize<D: ::serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                use ::serde::de::Error as _;
                let value = ::serde_json::Value::deserialize(d)?;
                let Some(tag) = value.get($tag).and_then(::serde_json::Value::as_str) else {
                    return Err(D::Error::custom(concat!(
                        "expected an object with a string `", $tag, "` field"
                    )));
                };
                match tag {
                    $($value => <$ty as ::serde::Deserialize>::deserialize(value)
                        .map(Self::$variant)
                        .map_err(D::Error::custom),)+
                    other => Err(D::Error::unknown_variant(other, Self::VALUES)),
                }
            }
        }
    };
}
pub(crate) use tagged_union;

/// A number that may arrive as a numeric string, a boolean or `null`, the way
/// a hand-written manifest tends to spell one. Serialises as a number.
pub fn coerce_u64<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    // 2^53 - 1 is exactly representable as a double.
    #[allow(clippy::cast_precision_loss, reason = "the constant is below 2^53")]
    const CEILING: f64 = MAX_SAFE_INTEGER as f64;
    let number = coerce_f64(d)?;
    if number.fract() != 0.0 || !(0.0..=CEILING).contains(&number) {
        return Err(de::Error::invalid_value(
            de::Unexpected::Float(number),
            &"a non-negative integer",
        ));
    }
    // Checked just above: integral, non-negative and below 2^53.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "range-checked"
    )]
    Ok(number as u64)
}

/// The floating-point half of [`coerce_u64`].
pub fn coerce_f64<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    struct Coerce;

    impl Visitor<'_> for Coerce {
        type Value = f64;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a number, or a string holding one")
        }

        fn visit_f64<E: de::Error>(self, v: f64) -> Result<f64, E> {
            Ok(v)
        }

        #[allow(
            clippy::cast_precision_loss,
            reason = "JSON numbers are already doubles"
        )]
        fn visit_i64<E: de::Error>(self, v: i64) -> Result<f64, E> {
            Ok(v as f64)
        }

        #[allow(
            clippy::cast_precision_loss,
            reason = "JSON numbers are already doubles"
        )]
        fn visit_u64<E: de::Error>(self, v: u64) -> Result<f64, E> {
            Ok(v as f64)
        }

        fn visit_bool<E: de::Error>(self, v: bool) -> Result<f64, E> {
            Ok(if v { 1.0 } else { 0.0 })
        }

        fn visit_unit<E: de::Error>(self) -> Result<f64, E> {
            Ok(0.0)
        }

        fn visit_none<E: de::Error>(self) -> Result<f64, E> {
            Ok(0.0)
        }

        fn visit_str<E: de::Error>(self, v: &str) -> Result<f64, E> {
            let trimmed = v.trim();
            if trimmed.is_empty() {
                return Ok(0.0);
            }
            trimmed
                .parse::<f64>()
                .ok()
                .filter(|n| n.is_finite())
                .ok_or_else(|| de::Error::invalid_value(de::Unexpected::Str(v), &self))
        }
    }

    d.deserialize_any(Coerce)
}

/// `true` as a type, for a response whose only shape is success.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct True;

impl Serialize for True {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_bool(true)
    }
}

impl<'de> Deserialize<'de> for True {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        if bool::deserialize(d)? {
            Ok(Self)
        } else {
            Err(de::Error::invalid_value(
                de::Unexpected::Bool(false),
                &"true",
            ))
        }
    }
}

impl JsonSchema for True {
    fn schema_name() -> Cow<'static, str> {
        "True".into()
    }

    fn inline_schema() -> bool {
        true
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({ "type": "boolean", "const": true })
    }
}

/// Validates every value of a string-keyed map, for `#[garde(custom(...))]`.
///
/// The map type has no `Validate` implementation of its own, and a rule that
/// stopped at the map would leave every entry of `agents.list` unchecked.
pub fn validate_map_values<V>(map: &IndexMap<String, V>, context: &()) -> garde::Result
where
    V: garde::Validate<Context = ()>,
{
    for value in map.values() {
        value.validate_with(context).map_err(|report| {
            garde::Error::new(
                report
                    .iter()
                    .map(|(path, error)| format!("{path}: {error}"))
                    .collect::<Vec<_>>()
                    .join("; "),
            )
        })?;
    }
    Ok(())
}

/// [`validate_map_values`] for a map whose entries may be `null`.
pub fn validate_map_options<V>(map: &IndexMap<String, Option<V>>, context: &()) -> garde::Result
where
    V: garde::Validate<Context = ()>,
{
    for value in map.values().flatten() {
        value.validate_with(context).map_err(|report| {
            garde::Error::new(
                report
                    .iter()
                    .map(|(path, error)| format!("{path}: {error}"))
                    .collect::<Vec<_>>()
                    .join("; "),
            )
        })?;
    }
    Ok(())
}

/// [`validate_map_options`] for a map that may itself be absent.
pub fn validate_optional_map_options<V>(
    map: &Option<IndexMap<String, Option<V>>>,
    context: &(),
) -> garde::Result
where
    V: garde::Validate<Context = ()>,
{
    match map {
        Some(map) => validate_map_options(map, context),
        None => Ok(()),
    }
}

/// `true`, as a serde default function.
pub fn yes() -> bool {
    true
}

/// Trims the characters JavaScript's `trim` does: Unicode white space plus the
/// byte-order mark, which the `White_Space` property leaves out.
pub fn js_trim(text: &str) -> &str {
    text.trim_matches(|c: char| c.is_whitespace() || c == '\u{FEFF}')
}
