;(function () {
    if (window.self !== window.top || window.__stremioShellFindFullscreenControl) return;

    // Stremio renders these icons without their names. Match both fullscreen
    // states by their SVG paths so translated titles do not affect discovery.
    var fullscreenPaths = [
        'M406.5 311.9v95.8h-95.8M105.5 202.5v-95.8h95.8M310.7 106.7h95.8v95.8M201.3 407.7h-95.8v-95.8',
        'M310.7 407.7v-95.8h95.8M201.3 106.7v95.8h-95.8M406.5 202.5h-95.8v-95.8M105.5 311.99h95.8v95.8'
    ].map(function (path) { return path.replace(/\s/g, ''); });

    window.__stremioShellFindFullscreenControl = function () {
        var elements = document.querySelectorAll('[title],[aria-label]');
        for (var i = 0; i < elements.length; i++) {
            var el = elements[i];
            var paths = el.querySelectorAll('svg path');
            var iconMatches = Array.prototype.some.call(paths, function (path) {
                return fullscreenPaths.indexOf((path.getAttribute('d') || '').replace(/\s/g, '')) !== -1;
            });
            var hint = (el.getAttribute('title') || '') + ' ' + (el.getAttribute('aria-label') || '');
            if (iconMatches || /fullscreen|full screen|pantalla completa/i.test(hint)) {
                var rect = el.getBoundingClientRect();
                if (rect.width > 0 && rect.height > 0) return el;
            }
        }
        return null;
    };
})();
