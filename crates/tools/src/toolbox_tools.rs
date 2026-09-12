//! A toolbox's programs, as tools the model can call directly.
//!
//! The default is not this. A toolbox advertises itself in one prompt section
//! — forty tokens for a box of two hundred programs, because the model already
//! knows what `curl` and `nmap` are. That is the right trade for a model that
//! reads its instructions.
//!
//! Not every model does. A model attends to its **tool schemas** far more
//! closely than to prose claiming it has capabilities — schemas are what
//! function-calling fine-tuning is *made of*, and prose about the environment
//! is not. Small models especially will read a paragraph saying "you can
//! search the web", read a tool list containing only `read_file` and `exec`,
//! and answer from the list: "the available tools only let me interact with
//! files." Observed, repeatedly, from a model holding a working search tool.
//!
//! So `expose: tools` materialises each declared entry as a real callable with
//! its own name and description. `ddgr` sits beside `read_file` in the list
//! the provider is sent, and a model that ignores prose cannot ignore it.
//!
//! The cost is real and is why this is opt-in per toolbox: roughly 60–80
//! tokens per entry, on every request of every turn. Seven entries is ~500
//! tokens against the ~40 the prompt section costs. Worth it for a model that
//! needs it; waste for one that does not.
//!
//! **Every generated tool runs through the exec guard and the same runner as
//! `exec`.** There is no second execution path: the tool is a *spelling* of
//! `exec` with the program fixed, so the binary allow-list, the argv contract,
//! the output cap and the container all apply unchanged. A toolbox cannot
//! grant reach by declaring an entry — it can only make reach the agent
//! already had easier to find.

use std::sync::Arc;

use ghostai_core::Result;
use ghostai_protocol::{
    TOOLBOX_DEFAULT_KEY, ToolAnnotations, ToolPermission, ToolPermissions, ToolRisk, Toolbox,
    ToolboxEntry, ToolboxExposure,
};
use ghostai_security::{ExecGuardOptions, guard_exec};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::argv::coerce_argv;
use crate::builtin::exec::render_run;
use crate::runner::RunRequest;
use crate::tool::{
    AnyTool, BoxFuture, ToolContext, ToolHandler, ToolOutput, ToolSpec, TypedTool,
    assert_not_aborted, is_tool_name, parameters_for,
};

/// The one argument every toolbox tool takes.
///
/// Always present, never defaulted — even for a program that takes none, where
/// the answer is `[]`. An optional field is one more thing for the model to
/// reason about, and "required, and sometimes empty" is a simpler contract
/// than "omit it unless you need it".
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ToolboxArgs {
    args: Vec<String>,
}

/// The `args` field, described in this entry's own words.
///
/// A description per entry rather than one shared sentence, because the
/// description is where the guidance has to land: it is the text a model is
/// looking at while it decides what to put in the field. A generic "arguments
/// as separate strings" says nothing about the flag this program insists on.
fn args_description(entry: &ToolboxEntry) -> String {
    let mut parts = vec![
        if entry.args.is_empty() {
            "Arguments as separate strings.".to_owned()
        } else {
            entry.args.clone()
        },
        "The program name is already supplied — do not repeat it.".to_owned(),
    ];
    if !entry.example.is_empty() {
        parts.push(format!(
            "Example: {}",
            serde_json::to_string(&entry.example).unwrap_or_default()
        ));
    }
    parts.join(" ")
}

struct ToolboxHandler {
    program: String,
}

impl ToolHandler for ToolboxHandler {
    type Args = ToolboxArgs;

    fn execute<'a>(
        &'a self,
        args: ToolboxArgs,
        ctx: &'a ToolContext,
    ) -> BoxFuture<'a, Result<ToolOutput>> {
        Box::pin(async move {
            assert_not_aborted(&ctx.token, &self.program)?;
            let mut command = vec![self.program.clone()];
            command.extend(args.args);
            let plan = guard_exec(
                &command,
                &ExecGuardOptions {
                    jail: &ctx.jail,
                    config: Some(&ctx.config.exec),
                    env: &ctx.env,
                    sandboxed: ctx.sandboxed,
                },
            )?;
            let outcome = ctx
                .runner
                .run(RunRequest {
                    timeout_ms: plan.timeout_ms,
                    plan: plan.clone(),
                    token: ctx.token.clone(),
                    clock: Arc::clone(&ctx.clock),
                    tee: None,
                })
                .await?;
            Ok(render_run(&command, &plan, &outcome))
        })
    }
}

/// One declared entry as a callable tool.
///
/// `None` for a name no provider would accept. A manifest is data, and a
/// program called `foo bar` or `../sh` would be rejected mid-turn as a provider
/// 400 that reads like the model is broken — so it is dropped here, where the
/// install review can say so, rather than advertised and refused later.
pub fn toolbox_tool(entry: &ToolboxEntry) -> Result<Option<AnyTool>> {
    if !is_tool_name(&entry.name) {
        return Ok(None);
    }

    // `use` alone. The toolbox is named once in the prompt section rather than
    // repeated in every one of these descriptions — that boilerplate is ~8
    // tokens times the number of entries, on every request of every turn, to
    // say something the model was already told.
    let description = if entry.r#use.is_empty() {
        format!("Run `{}`.", entry.name)
    } else {
        entry.r#use.clone()
    };

    // The derived schema, with this entry's own words on the `args` field and,
    // for a program that does nothing without an argument, a refusal of the
    // empty call — in the schema, where the model gets a validation message it
    // can act on rather than a usage error from the program and a dead end it
    // gives up at.
    let mut parameters = parameters_for::<ToolboxArgs>()?;
    if let Some(Value::Object(properties)) = parameters.get_mut("properties")
        && let Some(Value::Object(args)) = properties.get_mut("args")
    {
        args.insert(
            "description".to_owned(),
            Value::String(args_description(entry)),
        );
        if entry.requires_args {
            args.insert("minItems".to_owned(), Value::from(1));
        }
    }

    let spec = ToolSpec::new(entry.name.clone(), description)
        // The same band as `exec`, because it *is* `exec`. A toolbox entry that
        // claimed a gentler risk would be an approval prompt an operator
        // configured and then silently stopped seeing.
        .risk(ToolRisk::Exec)
        .annotations(ToolAnnotations {
            title: Some(format!("Run {}", entry.name)),
            read_only_hint: Some(false),
            destructive_hint: Some(true),
            idempotent_hint: None,
            open_world_hint: Some(true),
        });
    let handler = ToolboxHandler {
        program: entry.name.clone(),
    };
    // **Coerced before validation, not instead of it.** The advertised type
    // stays `string[]` — that is what the description asks for and what a
    // capable model sends — and a model that sends a string gets what it
    // evidently meant rather than a refusal it will answer with another broken
    // string.
    let tool = TypedTool::with_parameters(spec, parameters, handler)?.with_preprocess(Arc::new(
        |value: &mut Value| {
            if let Value::Object(fields) = value
                && let Some(args) = fields.get("args")
                && !args.is_array()
            {
                let coerced = coerce_argv(args).into_iter().map(Value::String).collect();
                fields.insert("args".to_owned(), Value::Array(coerced));
            }
        },
    ));
    Ok(Some(Arc::new(tool)))
}

/// Every entry a toolbox declares, as callables. Empty unless `expose: tools`.
pub fn toolbox_tools(toolbox: &Toolbox) -> Result<Vec<AnyTool>> {
    if toolbox.expose != ToolboxExposure::Tools {
        return Ok(Vec::new());
    }
    let mut tools = Vec::new();
    for entry in &toolbox.tools {
        if let Some(tool) = toolbox_tool(entry)? {
            tools.push(tool);
        }
    }
    Ok(tools)
}

/// One entry's permission for one agent: its override, the `*` default, the
/// manifest.
fn resolve_permission(entry: &ToolboxEntry, overrides: &ToolPermissions) -> ToolPermission {
    overrides
        .get(&entry.name)
        .or_else(|| overrides.get(TOOLBOX_DEFAULT_KEY))
        .copied()
        .unwrap_or(entry.permission)
}

/// The entries an agent is told about, which is not always what the box holds.
///
/// Separate from [`toolbox_permissions`] and deliberately blind to `expose`,
/// because the two answer different questions for different boxes. Under
/// `expose: tools` this is the prose beside the callables and the two must
/// list the same programs. Under `expose: prompt` there are no callables and
/// no permission map at all — every program is reached through `exec` — so
/// this prose *is* the whole mechanism, and an override that did nothing here
/// would do nothing anywhere.
///
/// Which makes the guarantee worth stating plainly: under `prompt` this is an
/// instruction, not a boundary. `exec` can still run a program left out of the
/// list, exactly as it can run one the manifest never declared. The boundary
/// is the container and the exec guard, and it has not moved.
pub fn visible_toolbox_entries<'a>(
    toolbox: &'a Toolbox,
    overrides: &ToolPermissions,
) -> Vec<&'a ToolboxEntry> {
    toolbox
        .tools
        .iter()
        .filter(|entry| resolve_permission(entry, overrides) != ToolPermission::Deny)
        .collect()
}

/// What each of those callables may do, for one agent.
///
/// Three sources, most specific first: the agent's override for that program,
/// the agent's `*` default, then the manifest's own `permission`. The result
/// is still only the *defaults* an agent's `tools` map is laid over, not a
/// ceiling.
///
/// A `deny` here is not a refusal at call time, it is an absence: `select`
/// drops a denied name from `definitions()`, so the model is never sent the
/// tool at all. That is the point — the cost this saves is the ~60–80 tokens
/// per entry the definition would take on every request.
///
/// Derived by the same name rule as [`toolbox_tools`] so the two cannot
/// disagree: an entry whose name no provider would accept is dropped from the
/// callables, and a permission for a tool that does not exist would read in
/// the settings UI as a row nothing can call.
pub fn toolbox_permissions(toolbox: &Toolbox, overrides: &ToolPermissions) -> ToolPermissions {
    let mut permissions = ToolPermissions::new();
    if toolbox.expose != ToolboxExposure::Tools {
        return permissions;
    }
    for entry in &toolbox.tools {
        if !is_tool_name(&entry.name) {
            continue;
        }
        permissions.insert(entry.name.clone(), resolve_permission(entry, overrides));
    }
    permissions
}
