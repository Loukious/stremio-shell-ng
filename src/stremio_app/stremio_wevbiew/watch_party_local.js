(function () {
    'use strict';
    if (window.self !== window.top || window.__stremioShellWatchPartyLocalInstalled) return;
    if (!window.chrome || !window.chrome.webview) return;
    window.__stremioShellWatchPartyLocalInstalled = true;

    var coreValue = window.core;
    var wrappedCore = null;
    var pending = null;
    var dispatchRevision = 0;
    var requestRevision = 0;
    var selectorRevision = 0;
    var selectionCounter = 0;
    var lastSelection = null;
    var attempts = 0;
    var documentId = Date.now().toString(36) + '-' + Math.random().toString(36).slice(2);

    function post(method, params) {
        window.chrome.webview.postMessage(JSON.stringify({ id: 1, args: [method, params] }));
    }

    function identifier(value) {
        return typeof value === 'string' && /^[A-Za-z0-9._:-]{1,256}$/.test(value)
            && /[A-Za-z0-9]/.test(value) && !/^(?:https?|file|stremio|magnet|data):/i.test(value);
    }

    function validMedia(media) {
        return media && typeof media === 'object'
            && typeof media.content_type === 'string'
            && /^[A-Za-z0-9_-]{1,64}$/.test(media.content_type)
            && identifier(media.metadata_id) && identifier(media.video_id);
    }

    function sameMedia(left, right) {
        return validMedia(left) && validMedia(right)
            && left.content_type === right.content_type
            && left.metadata_id === right.metadata_id && left.video_id === right.video_id;
    }

    function mediaFromSelected(selected) {
        if (!selected) return null;
        var meta = selected.metaRequest && selected.metaRequest.path;
        var stream = selected.streamRequest && selected.streamRequest.path;
        var video = stream || selected.subtitlesPath;
        if (!meta || !video || meta.type !== video.type) return null;
        var media = { content_type: meta.type, metadata_id: meta.id, video_id: video.id };
        return validMedia(media) ? media : null;
    }

    // This fingerprint stays in this WebView. URLs and addon settings are never
    // included in the native bridge payload or the watch-party invitation.
    function selectionFingerprint(selected, media) {
        var stream = selected && selected.stream;
        return JSON.stringify([media, stream && stream.url, stream && stream.infoHash,
            stream && stream.fileIdx, stream && stream.ytId, stream && stream.externalUrl]);
    }

    function reportSelection(selected, loadIntent) {
        var media = mediaFromSelected(selected);
        var fingerprint = selectionFingerprint(selected, media);
        // Core state can lag a Load/Unload dispatch. A snapshot must first
        // catch up to that dispatch before it can replace its local binding.
        if (!loadIntent && lastSelection && lastSelection.awaiting_snapshot) {
            if (lastSelection.fingerprint !== fingerprint) return;
            lastSelection.awaiting_snapshot = false;
        }
        if (loadIntent || !lastSelection || lastSelection.fingerprint !== fingerprint) {
            lastSelection = {
                fingerprint: fingerprint,
                selection_id: documentId + '-' + (++selectionCounter),
                generation: loadIntent && pending && sameMedia(media, pending.media) ? pending.generation : null,
                request_id: loadIntent && pending && sameMedia(media, pending.media) ? pending.request_id : null,
                awaiting_snapshot: !!loadIntent
            };
        }
        post('shell-watch-party-media', {
            media: media,
            selection_id: lastSelection.selection_id,
            generation: lastSelection.generation,
            request_id: lastSelection.request_id,
            load_intent: loadIntent
        });
    }

    function isPlayerLoad(action, model) {
        return action && action.action === 'Load' && action.args
            && action.args.model === 'Player' && action.args.args
            && (model === undefined || model === 'player');
    }

    function isPlayerUnload(action, model) {
        return action && action.action === 'Unload' && model === 'player';
    }

    function wrapCore(core) {
        if (!core || typeof core.dispatch !== 'function' || typeof core.getState !== 'function') return false;
        if (wrappedCore === core) return true;
        var original = core.dispatch;
        core.dispatch = function (action, model) {
            if (isPlayerLoad(action, model)) {
                dispatchRevision++;
                reportSelection(action.args.args, true);
            } else if (isPlayerUnload(action, model)) {
                dispatchRevision++;
                reportSelection(null, true);
            }
            return original.apply(this, arguments);
        };
        wrappedCore = core;
        return true;
    }

    async function reportCurrentMedia() {
        var core = window.core;
        if (!wrapCore(core)) return;
        var revision = ++requestRevision;
        var dispatchAtRequest = dispatchRevision;
        var selectorAtRequest = selectorRevision;
        try {
            var state = await core.getState('player');
            if (revision !== requestRevision || dispatchAtRequest !== dispatchRevision
                || selectorAtRequest !== selectorRevision || core !== window.core) return;
            reportSelection(state && state.selected, false);
        } catch (_) {
            if (revision === requestRevision && selectorAtRequest === selectorRevision && pending) {
                post('shell-watch-party-local-error', {
                    message: 'Stremio could not read the selected episode. Choose a stream again.'
                });
            }
        }
    }

    function connectCore() {
        if (wrapCore(window.core)) {
            reportCurrentMedia();
            return;
        }
        attempts++;
        if (attempts < 120) setTimeout(connectCore, 250);
        else if (pending) {
            post('shell-watch-party-local-error', {
                message: 'This Web interface does not expose local stream selection. Update Stremio Web and try again.'
            });
        }
    }

    // Current Web assigns window.core immediately before mounting React. Hook
    // that assignment so the first Player Load is captured ahead of loadfile.
    try {
        var descriptor = Object.getOwnPropertyDescriptor(window, 'core');
        if (!descriptor || (descriptor.configurable && !descriptor.get && !descriptor.set)) {
            Object.defineProperty(window, 'core', {
                configurable: true,
                enumerable: descriptor ? descriptor.enumerable : true,
                get: function () { return coreValue; },
                set: function (value) {
                    coreValue = value;
                    if (wrapCore(value)) reportCurrentMedia();
                }
            });
        }
    } catch (_) {}

    window.chrome.webview.addEventListener('message', function (event) {
        var data;
        try { data = typeof event.data === 'string' ? JSON.parse(event.data) : event.data; }
        catch (_) { return; }
        var args = data && data.args;
        if (!Array.isArray(args)) return;
        if (args[0] === 'shell-watch-party-media-get') {
            if (wrapCore(window.core)) reportCurrentMedia();
            return;
        }
        if (args[0] === 'shell-watch-party-local-clear') {
            pending = null;
            if (lastSelection) {
                lastSelection.generation = null;
                lastSelection.request_id = null;
            }
            selectorRevision++;
            requestRevision++;
            return;
        }
        if (args[0] !== 'shell-watch-party-select-source') return;
        var request = args[1];
        if (!request || !validMedia(request.media) || !Number.isSafeInteger(request.generation)
            || request.generation <= 0 || typeof request.request_id !== 'string'
            || request.request_id.length === 0 || request.request_id.length > 128) return;
        if (pending && request.generation < pending.generation) return;
        if (pending && request.request_id === pending.request_id && sameMedia(request.media, pending.media)) return;
        pending = {
            media: { content_type: request.media.content_type,
                metadata_id: request.media.metadata_id, video_id: request.media.video_id },
            generation: request.generation,
            request_id: request.request_id
        };
        selectorRevision++;
        requestRevision++;
        // This route contains only content IDs. Web loads the guest's own
        // profile/addons and presents its normal stream selector.
        window.location.hash = '#/metadetails/' + encodeURIComponent(pending.media.content_type)
            + '/' + encodeURIComponent(pending.media.metadata_id)
            + '/' + encodeURIComponent(pending.media.video_id);
    });

    connectCore();
})();
