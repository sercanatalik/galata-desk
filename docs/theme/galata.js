// galata-desk docs: a link back to the landing page at the top of the
// sidebar, as galata-vault's book has. Loaded by book.toml (additional-js).
// The book sits one level below the landing page (site/_build/docs/ under
// site/_build/), and mdBook sets path_to_root on every page.
(function () {
  function ready(fn) {
    if (document.readyState !== 'loading') fn();
    else document.addEventListener('DOMContentLoaded', fn);
  }
  ready(function () {
    var root = (typeof window.path_to_root === 'string') ? window.path_to_root : '';
    var box = document.querySelector('.sidebar-scrollbox');
    if (box && !box.querySelector('.gd-home')) {
      var home = document.createElement('a');
      home.className = 'gd-home';
      home.href = root + '../';
      home.textContent = '← galata desk';
      box.insertBefore(home, box.firstChild);
    }
  });
})();
