;(function () {
    if (window.self !== window.top || window.__stremioShellIntroSkipInstalled) return;
    window.__stremioShellIntroSkipInstalled = true;

    var state = { mode: 'auto', visible: false, kind: '', hidden: false, language: '', error: false, pendingMode: null };
    var hideTimer, closeTimer;
    var panelOpen = false;
    var settingsButton, skipButton, panel;
    var icon = '<svg viewBox="0 0 24 24" aria-hidden="true"><path fill="currentColor" d="M5 5v14l10-7L5 5zm12 0v14h2V5h-2z"/></svg>';

    function strings() {
        var es = /^(spa|es)([-_]|$)/i.test(state.language);
        return es ? {
            title: 'Saltar opening / ending', auto: 'Automático', manual: 'Mostrar botón (8 segundos)',
            intro: 'Saltar opening', outro: 'Saltar ending', error: 'No se pudo guardar la preferencia.'
        } : {
            title: 'Skip opening / ending', auto: 'Automatic', manual: 'Show button (8 seconds)',
            intro: 'Skip opening', outro: 'Skip ending', error: 'Could not save the preference.'
        };
    }

    function post(method, params) {
        window.chrome.webview.postMessage(JSON.stringify({ id: 1, args: [method, params || {}] }));
    }

    function isPlayer() {
        return (location.hash || '').indexOf('/player') !== -1;
    }

    function closePanel(restoreFocus) {
        if (!panel || !panelOpen) return;
        panelOpen = false;
        panel.inert = true;
        panel.classList.remove('fade-enter');
        panel.classList.add('fade-active', 'fade-exit');
        settingsButton.setAttribute('aria-expanded', 'false');
        if (restoreFocus) settingsButton.focus();
        // Use the frontend's fade duration; keep the node mounted until exit completes.
        var duration = parseFloat(getComputedStyle(panel).transitionDuration) * 1000 || 0;
        closeTimer = setTimeout(function () { if (!panelOpen) panel.hidden = true; }, duration);
    }

    function openPanel(focusOption) {
        clearTimeout(closeTimer);
        panelOpen = true;
        panel.inert = false;
        panel.hidden = false;
        panel.className = 'fade-enter';
        settingsButton.setAttribute('aria-expanded', 'true');
        render();
        positionPanel();
        requestAnimationFrame(function () { if (panelOpen) panel.classList.add('fade-active'); });
        if (focusOption) panel.querySelector('input:checked').focus();
        post('shell-intro-skip-mode-get');
    }

    function render() {
        if (!panel) return;
        var copy = strings();
        settingsButton.title = copy.title;
        settingsButton.setAttribute('aria-label', copy.title);
        panel.querySelector('legend').textContent = copy.title;
        panel.querySelector('[data-label="auto"]').textContent = copy.auto;
        panel.querySelector('[data-label="manual"]').textContent = copy.manual;
        panel.querySelector('[role="alert"]').textContent = state.error ? copy.error : '';
        panel.setAttribute('aria-busy', state.pendingMode !== null ? 'true' : 'false');
        panel.querySelectorAll('input').forEach(function (input) {
            input.checked = input.value === state.mode;
            input.closest('label').setAttribute('data-selected', input.checked ? 'true' : 'false');
            input.disabled = state.pendingMode !== null;
        });
        skipButton.querySelector('span').textContent = copy[state.kind] || copy.intro;
        skipButton.hidden = !isPlayer() || !state.visible || state.hidden;
    }

    function positionPanel() {
        var rect = settingsButton.getBoundingClientRect();
        var box = panel.getBoundingClientRect();
        panel.style.left = Math.max(14, Math.min(rect.right - box.width, window.innerWidth - box.width - 14)) + 'px';
        panel.style.top = Math.max(14, Math.min(rect.bottom + 10, window.innerHeight - box.height - 14)) + 'px';
    }

    function findAnchor() {
        var pip = document.getElementById('stremio-shell-pip-btn');
        if (pip && pip.isConnected) return pip;
        var elements = document.querySelectorAll('[title],[aria-label]');
        for (var i = 0; i < elements.length; i++) {
            var el = elements[i];
            var hint = (el.getAttribute('title') || '') + ' ' + (el.getAttribute('aria-label') || '');
            if (/fullscreen|full screen|pantalla completa/i.test(hint) && el.getBoundingClientRect().width > 0) return el;
        }
        return null;
    }

    function install(anchor) {
        var style = document.createElement('style');
        style.textContent = `
            .shell-intro-control { border:0; font:inherit; color:var(--primary-foreground-color); cursor:pointer; }
            .shell-intro-control[hidden], #stremio-shell-intro-skip-panel[hidden] { display:none!important; }
            .shell-intro-control:focus-visible, #stremio-shell-intro-skip-panel input:focus-visible {
                outline:var(--focus-outline-size, 2px) solid var(--primary-foreground-color); outline-offset:2px;
            }
            #stremio-shell-intro-skip-settings-btn { background:transparent; min-width:44px; min-height:44px; }
            #stremio-shell-intro-skip-settings-btn:hover { background:var(--overlay-color); }
            #stremio-shell-intro-skip-settings-btn svg { width:2.2rem; height:2.2rem; }
            #stremio-shell-manual-skip-btn {
                position:fixed; right:2rem; bottom:7rem; z-index:10; display:flex; align-items:center;
                gap:0.75rem; min-height:44px; padding:0.75rem 1rem; border-radius:var(--border-radius);
                background:var(--modal-background-color); max-width:calc(100vw - 2rem);
            }
            #stremio-shell-manual-skip-btn:hover { background:var(--secondary-background-color); }
            #stremio-shell-manual-skip-btn svg { width:1.4rem; height:1.4rem; flex:none; }
            #stremio-shell-intro-skip-panel {
                position:fixed; z-index:11; box-sizing:border-box; margin:0; padding:1rem; border:0;
                width:20rem; max-width:calc(100vw - 28px); border-radius:var(--border-radius);
                color:var(--primary-foreground-color); background:var(--modal-background-color); font:inherit;
            }
            #stremio-shell-intro-skip-panel legend { float:left; width:100%; padding:0; margin-bottom:0.5rem; }
            #stremio-shell-intro-skip-panel label { position:relative; clear:both; display:flex; align-items:center; gap:0.75rem; min-height:44px; padding:0 0.5rem; border-radius:var(--border-radius); cursor:pointer; }
            #stremio-shell-intro-skip-panel label:hover,
            #stremio-shell-intro-skip-panel label[data-selected="true"] { background:var(--overlay-color); }
            #stremio-shell-intro-skip-panel label:has(input:focus-visible) { outline:var(--focus-outline-size, 2px) solid var(--primary-foreground-color); outline-offset:-2px; }
            #stremio-shell-intro-skip-panel input { display:block; position:absolute; width:1px; height:1px; margin:0; opacity:0; clip-path:inset(50%); }
            #stremio-shell-intro-skip-panel .skip-mode-check { width:1.25rem; height:1.25rem; flex:none; fill:currentColor; visibility:hidden; }
            #stremio-shell-intro-skip-panel label[data-selected="true"] .skip-mode-check { visibility:visible; }
            #stremio-shell-intro-skip-panel [role="alert"]:empty { display:none; }
            @media (max-width:720px) { #stremio-shell-manual-skip-btn { right:1rem; bottom:6rem; } }
        `;
        document.head.appendChild(style);
        settingsButton = document.createElement('button');
        settingsButton.type = 'button';
        settingsButton.id = 'stremio-shell-intro-skip-settings-btn';
        settingsButton.className = anchor.className + ' shell-intro-control';
        settingsButton.innerHTML = icon;
        settingsButton.setAttribute('aria-controls', 'stremio-shell-intro-skip-panel');
        settingsButton.setAttribute('aria-expanded', 'false');
        settingsButton.addEventListener('click', function (event) {
            event.stopPropagation();
            if (panelOpen) closePanel(false);
            else openPanel(event.detail === 0);
        });

        panel = document.createElement('fieldset');
        panel.id = 'stremio-shell-intro-skip-panel';
        panel.hidden = true;
        var check = '<svg class="skip-mode-check" viewBox="0 0 24 24" aria-hidden="true"><path d="M9 16.2 4.8 12l-1.4 1.4L9 19 21 7l-1.4-1.4z"/></svg>';
        panel.innerHTML = '<legend></legend>'
            + '<label><input type="radio" name="shell-intro-mode" value="auto">' + check + '<span data-label="auto"></span></label>'
            + '<label><input type="radio" name="shell-intro-mode" value="manual">' + check + '<span data-label="manual"></span></label>'
            + '<div role="alert"></div>';
        panel.addEventListener('click', function (event) {
            if (event.target.name !== 'shell-intro-mode') return;
            if (event.target.value === state.mode) return closePanel(true);
            state.pendingMode = event.target.value;
            state.error = false;
            render();
            post('shell-set-intro-skip-mode', { mode: event.target.value });
        });
        panel.addEventListener('click', function (event) { event.stopPropagation(); });
        document.body.appendChild(panel);

        skipButton = document.createElement('button');
        skipButton.id = 'stremio-shell-manual-skip-btn';
        skipButton.type = 'button';
        skipButton.className = 'shell-intro-control';
        skipButton.innerHTML = icon + '<span></span>';
        skipButton.addEventListener('click', function (event) {
            event.stopPropagation();
            post('shell-manual-skip');
            state.hidden = true;
            render();
        });
        document.body.appendChild(skipButton);
        render();
    }

    function placeButton() {
        if (!isPlayer()) {
            if (settingsButton) settingsButton.remove();
            closePanel(false);
            return;
        }
        var anchor = findAnchor();
        if (!anchor || !anchor.parentElement) return;
        if (!settingsButton) install(anchor);
        if (settingsButton.parentElement !== anchor.parentElement || settingsButton.nextElementSibling !== anchor) {
            settingsButton.className = anchor.className + ' shell-intro-control';
            anchor.parentElement.insertBefore(settingsButton, anchor);
        }
    }

    window.chrome.webview.addEventListener('message', function (event) {
        var data;
        try { data = typeof event.data === 'string' ? JSON.parse(event.data) : event.data; } catch (_) { return; }
        var args = data && data.args;
        if (!Array.isArray(args)) return;
        var next = args[1] || {};
        if (args[0] === 'shell-manual-skip-state') {
            clearTimeout(hideTimer);
            state.visible = !!next.visible;
            state.kind = next.kind;
            state.hidden = false;
            if (state.visible) hideTimer = setTimeout(function () { state.hidden = true; render(); }, 8000);
        } else if (args[0] === 'shell-intro-skip-mode-state') {
            if (next.mode === 'auto' || next.mode === 'manual') state.mode = next.mode;
            state.error = !!next.error;
            if (state.pendingMode !== null && (state.error || state.mode === state.pendingMode)) {
                state.pendingMode = null;
                if (!state.error) closePanel(true);
            }
        } else return;
        render();
    });

    document.addEventListener('mousedown', function (event) {
        if (panel && !panel.contains(event.target) && !settingsButton.contains(event.target)) closePanel(false);
    }, true);
    // Let native buttons/radios handle activation before the shell's MPV shortcuts.
    document.addEventListener('keydown', function (event) {
        if (panelOpen && event.key === 'Escape') {
            event.preventDefault();
            event.stopImmediatePropagation();
            closePanel(true);
        } else if (event.target.closest('.shell-intro-control, #stremio-shell-intro-skip-panel')) {
            event.stopImmediatePropagation();
        }
    }, true);
    window.addEventListener('hashchange', function () {
        state.visible = false;
        clearTimeout(hideTimer);
        closePanel(false);
        render();
        placeButton();
    });
    window.addEventListener('resize', function () { if (panelOpen) positionPanel(); });

    async function refresh() {
        placeButton();
        if (!window.core || typeof window.core.getState !== 'function') return;
        try {
            var ctx = await window.core.getState('ctx');
            var language = ctx && ctx.profile && ctx.profile.settings && ctx.profile.settings.interfaceLanguage;
            if (language !== state.language) { state.language = language || ''; render(); }
        } catch (_) {}
    }
    setInterval(refresh, 1000);
    new MutationObserver(placeButton).observe(document.documentElement, { childList: true, subtree: true });
    refresh();
    post('shell-intro-skip-mode-get');
})();
