'use strict';
(() => {
  const pending = new WeakMap();

  function begin(element, mode, label, valid = () => true) {
    if (!element) return () => {};
    pending.get(element)?.();
    element.setAttribute('aria-busy', 'true');
    const timer = setTimeout(() => {
      if (!valid()) { finish(); return; }
      element.setAttribute('data-loading', mode);
      element.setAttribute('data-loading-label', label);
    }, 150);

    function finish() {
      clearTimeout(timer);
      if (pending.get(element) !== finish) return;
      pending.delete(element);
      element.removeAttribute('aria-busy');
      element.removeAttribute('data-loading');
      element.removeAttribute('data-loading-label');
    }

    pending.set(element, finish);
    return finish;
  }

  window.archiveLoading = {begin, clear: element => pending.get(element)?.()};
})();
