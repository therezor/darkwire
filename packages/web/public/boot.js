/*
 * The pre-paint stamp: theme, language and text direction on `<html>`.
 *
 * A classic blocking script in `<head>`, on purpose. It has to run before the
 * first paint, and anything deferred, bundled or async paints the default
 * theme first and corrects it a frame later. That frame is the flash. It sits
 * in `public/` so Vite ships it as-is, next to `index.html`, instead of
 * bundling it into the deferred module graph.
 *
 * It duplicates `src/theme/theme.ts` and `src/i18n/locale-preference.ts` in
 * about ten lines each because it cannot import them. `theme.test.ts` runs
 * this file against a stubbed DOM and asserts it agrees with both modules on
 * every combination of stored preference and OS setting, so the duplication
 * cannot drift silently.
 *
 * The locale pays for itself twice: `lang` is what a screen reader picks its
 * voice from, so correcting it a frame later means the first announcement is
 * in the wrong one, and `dir` is layout, so a late correction is a visible
 * reflow rather than a flash.
 */
(function () {
  var storage = null;
  try {
    storage = localStorage;
  } catch (error) {
    /* Storage is unreachable in a cross-origin iframe. Fall back to the OS. */
  }

  var read = function (key) {
    try {
      return storage && storage.getItem(key);
    } catch (error) {
      return null;
    }
  };

  var stored = read('darkwire.theme');
  var light =
    stored === 'light' ||
    (stored !== 'dark' &&
      window.matchMedia('(prefers-color-scheme: light)').matches);

  document.documentElement.dataset.theme = light ? 'light' : 'dark';

  /* `system`, or absent, means "ask the browser": the same three-state
     preference the theme has, resolved the same way. Only the language
     subtag is compared, so `de-AT` matches a `de` bundle. */
  var locale = read('darkwire.locale');
  var supported = ['en'];
  var wanted =
    locale && locale !== 'system'
      ? [locale]
      : (navigator.languages || []).slice();

  var resolved = 'en';
  for (var i = 0; i < wanted.length; i += 1) {
    var tag = String(wanted[i]).toLowerCase().replace(/_/g, '-');
    for (var j = 0; j < supported.length; j += 1) {
      if (tag === supported[j] || tag.indexOf(supported[j] + '-') === 0) {
        resolved = supported[j];
        i = wanted.length;
        break;
      }
    }
  }

  document.documentElement.lang = resolved;
  document.documentElement.dir =
    ['ar', 'fa', 'he', 'ps', 'ur', 'yi'].indexOf(resolved.split('-')[0]) === -1
      ? 'ltr'
      : 'rtl';
})();
