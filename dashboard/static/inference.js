(function () {
    const IMAGE_ALLOWED_MIME_TYPES = new Set(['image/jpeg', 'image/png', 'image/webp']);
    const IMAGE_SOURCE_MAX_BYTES = 10 * 1024 * 1024;
    const IMAGE_TARGET_DATA_URL_BYTES = 5 * 1024 * 1024;
    const IMAGE_HARD_DATA_URL_BYTES = 6 * 1024 * 1024;
    const IMAGE_MAX_EDGE_PX = 1600;
    const IMAGE_EXPORT_QUALITIES = [0.92, 0.82, 0.72, 0.62];
    const DEFAULT_TIMEOUT_SECONDS = 60;
    const IMAGE_TIMEOUT_MIN_SECONDS = 180;
    const IMAGE_TIMEOUT_MAX_SECONDS = 420;
    const IMAGE_TIMEOUT_PER_MIB_SECONDS = 30;
    const AUDIO_TIMEOUT_MIN_SECONDS = 180;
    const AUDIO_TIMEOUT_MAX_SECONDS = 600;
    const AUDIO_TIMEOUT_PER_MIB_SECONDS = 20;
    const AUDIO_ALLOWED_MIME_TYPES = new Set([
        'audio/flac',
        'audio/m4a',
        'audio/mp3',
        'audio/mp4',
        'audio/mpeg',
        'audio/ogg',
        'audio/wav',
        'audio/wave',
        'audio/vnd.wave',
        'audio/webm',
        'audio/x-flac',
        'audio/x-m4a',
        'audio/x-mp3',
        'audio/x-wav',
    ]);
    const AUDIO_MIME_TYPE_EXTENSIONS = {
        'audio/flac': '.flac',
        'audio/m4a': '.m4a',
        'audio/mp3': '.mp3',
        'audio/mp4': '.mp4',
        'audio/mpeg': '.mp3',
        'audio/ogg': '.ogg',
        'audio/wav': '.wav',
        'audio/wave': '.wav',
        'audio/vnd.wave': '.wav',
        'audio/webm': '.webm',
        'audio/x-flac': '.flac',
        'audio/x-m4a': '.m4a',
        'audio/x-mp3': '.mp3',
        'audio/x-wav': '.wav',
    };

    function setElementText(target, text) {
        if (!target) {
            return;
        }
        if (typeof target.value === 'string') {
            target.value = text;
            return;
        }
        target.textContent = text;
    }

    function appendElementText(target, text) {
        if (!target) {
            return;
        }
        if (typeof target.value === 'string') {
            target.value += text;
            return;
        }
        target.textContent += text;
    }

    function setBusyState(button, processing, runLabel, isBusy) {
        if (button) {
            button.disabled = isBusy;
        }
        if (processing) {
            processing.style.display = isBusy ? '' : 'none';
            if (isBusy) {
                processing.style.width = '15px';
                processing.style.height = '15px';
            }
        }
        if (runLabel) {
            runLabel.style.display = isBusy ? 'none' : '';
        }
    }

    function normalizePathBase(value) {
        return String(value || '').replace(/\/+$/, '');
    }

    function normalizeApiTypeValue(value) {
        return String(value || '').trim().toLowerCase();
    }

    function isTranscriptionApiType(value) {
        return normalizeApiTypeValue(value) === 'transcriptions';
    }

    function buildUrlFromSegments(basePath, segments) {
        const prefix = normalizePathBase(basePath);
        const encodedPath = segments
            .flatMap((segment) => String(segment || '').split('/'))
            .filter((segment) => segment !== '')
            .map((segment) => encodeURIComponent(segment))
            .join('/');
        return new URL(prefix + '/' + encodedPath, window.location.origin).toString();
    }

    function cloneMapValue(value) {
        if (value == null || typeof value !== 'object') {
            return value;
        }
        return JSON.parse(JSON.stringify(value));
    }

    function normalizeOpenAIRequestMap(value) {
        const nextMap = cloneMapValue(value) || {};
        if (!nextMap || typeof nextMap !== 'object' || Array.isArray(nextMap)) {
            return nextMap;
        }

        function parseBooleanString(raw) {
            if (typeof raw === 'boolean') {
                return raw;
            }
            if (typeof raw !== 'string') {
                return raw;
            }
            const normalized = raw.trim().toLowerCase();
            if (normalized === 'true') {
                return true;
            }
            if (normalized === 'false') {
                return false;
            }
            return raw;
        }

        function parseNumberString(raw) {
            if (typeof raw === 'number') {
                return Number.isFinite(raw) ? raw : raw;
            }
            if (typeof raw !== 'string') {
                return raw;
            }
            const normalized = raw.trim();
            if (normalized === '') {
                return raw;
            }
            if (!/^-?(?:\d+|\d+\.\d+|\.\d+)$/.test(normalized)) {
                return raw;
            }
            const parsed = Number(normalized);
            return Number.isFinite(parsed) ? parsed : raw;
        }

        ['stream'].forEach(function (key) {
            if (Object.prototype.hasOwnProperty.call(nextMap, key)) {
                nextMap[key] = parseBooleanString(nextMap[key]);
            }
        });

        ['max_tokens', 'maxTokens', 'temperature', 'top_p', 'topP', 'n'].forEach(function (key) {
            if (Object.prototype.hasOwnProperty.call(nextMap, key)) {
                nextMap[key] = parseNumberString(nextMap[key]);
            }
        });

        return nextMap;
    }

    function mergeHeaders(baseHeaders, extraHeaders) {
        const merged = Object.assign({}, baseHeaders || {});
        if (!extraHeaders || typeof extraHeaders !== 'object') {
            return merged;
        }
        Object.entries(extraHeaders).forEach(function ([key, value]) {
            if (value == null) {
                return;
            }
            merged[String(key)] = String(value);
        });
        return merged;
    }

    function formatLatencyValue(value) {
        const text = String(value || '').trim();
        return text === '' ? '-' : text;
    }

    function formatBytes(bytes) {
        if (!Number.isFinite(bytes) || bytes <= 0) {
            return '0 MiB';
        }
        return (bytes / (1024 * 1024)).toFixed(1) + ' MiB';
    }

    function inferImageMimeType(mimeType, fallbackName) {
        const normalizedType = String(mimeType || '').split(';', 1)[0].trim().toLowerCase();
        if (IMAGE_ALLOWED_MIME_TYPES.has(normalizedType)) {
            return normalizedType;
        }

        const normalizedName = String(fallbackName || '').trim().toLowerCase();
        if (normalizedName.endsWith('.jpg') || normalizedName.endsWith('.jpeg')) {
            return 'image/jpeg';
        }
        if (normalizedName.endsWith('.png')) {
            return 'image/png';
        }
        if (normalizedName.endsWith('.webp')) {
            return 'image/webp';
        }
        return '';
    }

    function dataUrlSizeBytes(dataUrl) {
        return new Blob([String(dataUrl || '')]).size;
    }

    function loadBlobAsImage(blob) {
        return new Promise(function (resolve, reject) {
            const objectUrl = URL.createObjectURL(blob);
            const image = new Image();

            image.onload = function () {
                URL.revokeObjectURL(objectUrl);
                resolve(image);
            };
            image.onerror = function () {
                URL.revokeObjectURL(objectUrl);
                reject(new Error('Image could not be decoded.'));
            };
            image.src = objectUrl;
        });
    }

    async function normalizeImageBlob(blob, sourceLabel) {
        const effectiveType = inferImageMimeType(blob && blob.type, sourceLabel);
        if (!effectiveType) {
            throw new Error('Unsupported image type. Use JPEG, PNG, or WebP.');
        }

        if (!blob || blob.size > IMAGE_SOURCE_MAX_BYTES) {
            throw new Error('Image is too large. The source file must be 10 MiB or smaller.');
        }

        const image = await loadBlobAsImage(blob);
        const width = image.naturalWidth || image.width;
        const height = image.naturalHeight || image.height;
        if (!width || !height) {
            throw new Error('Image could not be decoded.');
        }

        const longestEdge = Math.max(width, height);
        const scale = longestEdge > IMAGE_MAX_EDGE_PX ? (IMAGE_MAX_EDGE_PX / longestEdge) : 1;
        const targetWidth = Math.max(1, Math.round(width * scale));
        const targetHeight = Math.max(1, Math.round(height * scale));

        const canvas = document.createElement('canvas');
        canvas.width = targetWidth;
        canvas.height = targetHeight;
        const context = canvas.getContext('2d');
        if (!context) {
            throw new Error('Browser could not prepare the image for upload.');
        }

        let bestCandidate = null;
        const exportTypes = effectiveType === 'image/png'
            ? ['image/png', 'image/jpeg']
            : [effectiveType].concat(effectiveType === 'image/jpeg' ? [] : ['image/jpeg']);

        for (const exportType of exportTypes) {
            const qualities = exportType === 'image/png' ? [null] : IMAGE_EXPORT_QUALITIES;
            for (const quality of qualities) {
                if (exportType === 'image/jpeg') {
                    context.fillStyle = '#ffffff';
                    context.fillRect(0, 0, targetWidth, targetHeight);
                } else {
                    context.clearRect(0, 0, targetWidth, targetHeight);
                }
                context.drawImage(image, 0, 0, targetWidth, targetHeight);

                const dataUrl = quality == null
                    ? canvas.toDataURL(exportType)
                    : canvas.toDataURL(exportType, quality);
                const sizeBytes = dataUrlSizeBytes(dataUrl);
                const candidate = {
                    dataUrl: dataUrl,
                    sizeBytes: sizeBytes,
                    width: targetWidth,
                    height: targetHeight,
                    mimeType: exportType,
                };

                if (!bestCandidate || sizeBytes < bestCandidate.sizeBytes) {
                    bestCandidate = candidate;
                }
                if (sizeBytes <= IMAGE_TARGET_DATA_URL_BYTES) {
                    return candidate;
                }
            }
        }

        if (bestCandidate && bestCandidate.sizeBytes <= IMAGE_HARD_DATA_URL_BYTES) {
            return bestCandidate;
        }

        throw new Error(
            'Image is still too large after normalization. Keep the encoded upload at 6 MiB or less.'
        );
    }

    async function fetchRemoteImageBlob(remoteFetchPath, url, signal) {
        let response;
        try {
            response = await fetch(new URL(remoteFetchPath, window.location.origin).toString(), {
                method: 'POST',
                headers: {
                    'Accept': 'application/octet-stream',
                    'Content-Type': 'application/json',
                },
                body: JSON.stringify({ url: url }),
                signal: signal,
            });
        } catch (error) {
            if (error instanceof DOMException && error.name === 'AbortError') {
                throw error;
            }
            throw new Error(
                'Dashboard could not fetch the remote image. Download the image and upload it instead.'
            );
        }

        if (!response.ok) {
            let responseMessage = '';
            try {
                responseMessage = String(await response.text() || '').trim();
            } catch (_error) {
                responseMessage = '';
            }
            throw new Error(
                responseMessage || ('Dashboard failed to fetch the remote image with HTTP ' + response.status + '.')
            );
        }

        let fetchedBlob;
        try {
            fetchedBlob = await response.blob();
        } catch (error) {
            if (error instanceof DOMException && error.name === 'AbortError') {
                throw error;
            }
            throw new Error(
                'Remote URL fetch could not read the full image body. Download the image and upload it instead.'
            );
        }
        const effectiveType = inferImageMimeType(
            response.headers.get('Content-Type') || fetchedBlob.type,
            url
        );
        if (!effectiveType) {
            throw new Error('Remote URL did not return a supported image type. Use JPEG, PNG, or WebP.');
        }

        if (effectiveType === fetchedBlob.type) {
            return fetchedBlob;
        }
        return new Blob([fetchedBlob], { type: effectiveType });
    }

    function computeRequestTimeoutSeconds(apiType, payloadBytes) {
        if (apiType !== 'image2text' && !isTranscriptionApiType(apiType)) {
            return String(DEFAULT_TIMEOUT_SECONDS);
        }

        const sizeBytes = Math.max(1, Number(payloadBytes) || 0);
        const sizeMiB = Math.max(1, Math.ceil(sizeBytes / (1024 * 1024)));
        if (apiType === 'image2text') {
            const timeoutSeconds = Math.max(
                IMAGE_TIMEOUT_MIN_SECONDS,
                DEFAULT_TIMEOUT_SECONDS + (sizeMiB * IMAGE_TIMEOUT_PER_MIB_SECONDS)
            );
            return String(Math.min(IMAGE_TIMEOUT_MAX_SECONDS, timeoutSeconds));
        }

        const timeoutSeconds = Math.max(
            AUDIO_TIMEOUT_MIN_SECONDS,
            DEFAULT_TIMEOUT_SECONDS + (sizeMiB * AUDIO_TIMEOUT_PER_MIB_SECONDS)
        );
        return String(Math.min(AUDIO_TIMEOUT_MAX_SECONDS, timeoutSeconds));
    }

    function createInferenceController(config) {
        const context = config || {};
        let abortController = null;
        let inferenceInFlight = false;
        let firstOutputNotified = false;
        let previewObjectUrl = null;
        let audioPreviewObjectUrl = null;

        function getElement(id) {
            return document.getElementById(id);
        }

        function getPromptValue() {
            return String((getElement('prompt') || {}).value || '');
        }

        function isTranscriptionRequest() {
            const normalizedPath = String(context.sampleQueryPath || '')
                .trim()
                .replace(/^\/+|\/+$/g, '')
                .toLowerCase();
            return normalizedPath === 'v1/audio/transcriptions' || isTranscriptionApiType(context.apiType);
        }

        function getEffectiveTranscriptionPrompt() {
            const prompt = getPromptValue().trim();
            const samplePrompt = String(context.samplePrompt || '').trim();
            if (prompt === '' || (samplePrompt !== '' && prompt === samplePrompt)) {
                return '';
            }
            return prompt;
        }

        function clearPreviewObjectUrl() {
            if (previewObjectUrl) {
                URL.revokeObjectURL(previewObjectUrl);
                previewObjectUrl = null;
            }
        }

        function clearAudioPreviewObjectUrl() {
            if (audioPreviewObjectUrl) {
                URL.revokeObjectURL(audioPreviewObjectUrl);
                audioPreviewObjectUrl = null;
            }
        }

        function clearImagePreview() {
            const preview = getElement('preview');
            clearPreviewObjectUrl();
            if (!preview) {
                return;
            }

            preview.style.display = 'none';
            preview.removeAttribute('src');
            preview.onload = null;
            preview.onerror = null;
        }

        function clearAudioPreview() {
            const audioPreview = getElement('audioPreview');
            clearAudioPreviewObjectUrl();
            if (!audioPreview) {
                return;
            }

            audioPreview.pause();
            audioPreview.style.display = 'none';
            audioPreview.removeAttribute('src');
            audioPreview.load();
        }

        function setImageSourceStatus(message, tone) {
            const target = getElement('imageSourceStatus');
            if (!target) {
                return;
            }

            target.textContent = String(message || '');
            if (tone === 'error') {
                target.style.color = '#b42318';
                return;
            }
            if (tone === 'warning') {
                target.style.color = '#b54708';
                return;
            }
            target.style.color = '#475467';
        }

        function setAudioSourceStatus(message, tone) {
            const target = getElement('audioSourceStatus');
            if (!target) {
                return;
            }

            target.textContent = String(message || '');
            if (tone === 'error') {
                target.style.color = '#b42318';
                return;
            }
            if (tone === 'warning') {
                target.style.color = '#b54708';
                return;
            }
            target.style.color = '#475467';
        }

        function getSelectedImageFile() {
            const fileInput = getElement('imageFileInput');
            if (!fileInput || !fileInput.files || fileInput.files.length === 0) {
                return null;
            }
            return fileInput.files[0];
        }

        function getSelectedAudioFile() {
            const fileInput = getElement('audioFileInput');
            if (!fileInput || !fileInput.files || fileInput.files.length === 0) {
                return null;
            }
            return fileInput.files[0];
        }

        function getSelectedImageSourceType() {
            const selected = document.querySelector('input[name="imageSourceType"]:checked');
            return selected ? String(selected.value || 'file') : 'file';
        }

        function getSelectedAudioSourceType() {
            const selected = document.querySelector('input[name="audioSourceType"]:checked');
            return selected ? String(selected.value || 'file') : 'file';
        }

        function getUrlInputValue() {
            return String((getElement('urlInput') || {}).value || '').trim();
        }

        function buildRemoteImageFetchUrl() {
            return String(context.remoteImageFetchPath || '/image2text/fetch-remote');
        }

        function buildRemoteAudioFetchUrl() {
            return String(context.remoteAudioFetchPath || '/audio/fetch-remote');
        }

        function syncImageSourceControls() {
            if (context.apiType !== 'image2text') {
                return;
            }

            const sourceType = getSelectedImageSourceType();
            const imageFileInput = getElement('imageFileInput');
            const urlInput = getElement('urlInput');
            const uploadSection = getElement('imageUploadSection');
            const urlSection = getElement('imageUrlSection');

            if (imageFileInput) {
                imageFileInput.disabled = sourceType !== 'file';
            }
            if (urlInput) {
                urlInput.disabled = sourceType !== 'url';
            }
            if (uploadSection) {
                uploadSection.style.opacity = sourceType === 'file' ? '1' : '0.55';
            }
            if (urlSection) {
                urlSection.style.opacity = sourceType === 'url' ? '1' : '0.55';
            }
        }

        function syncAudioSourceControls() {
            if (!isTranscriptionRequest()) {
                return;
            }

            const sourceType = getSelectedAudioSourceType();
            const audioFileInput = getElement('audioFileInput');
            const urlInput = getElement('urlInput');
            const uploadSection = getElement('audioUploadSection');
            const urlSection = getElement('audioUrlSection');

            if (audioFileInput) {
                audioFileInput.disabled = sourceType !== 'file';
            }
            if (urlInput) {
                urlInput.disabled = sourceType !== 'url';
            }
            if (uploadSection) {
                uploadSection.style.opacity = sourceType === 'file' ? '1' : '0.55';
            }
            if (urlSection) {
                urlSection.style.opacity = sourceType === 'url' ? '1' : '0.55';
            }
        }

        function updateImageSourceStatus() {
            if (context.apiType !== 'image2text') {
                return;
            }

            const sourceType = getSelectedImageSourceType();
            const selectedFile = getSelectedImageFile();
            if (sourceType === 'file' && selectedFile) {
                setImageSourceStatus(
                    'Active source: uploaded file "' + selectedFile.name + '" (' + formatBytes(selectedFile.size) + ').',
                    'info'
                );
                return;
            }

            const imageUrl = getUrlInputValue();
            if (sourceType === 'url' && imageUrl !== '') {
                setImageSourceStatus(
                    'Active source: remote URL via dashboard fetch. Click Preview or Run to fetch it.',
                    'warning'
                );
                return;
            }

            if (sourceType === 'file') {
                setImageSourceStatus(
                    'Active source: uploaded file. Select a local image to continue.',
                    'info'
                );
                return;
            }

            setImageSourceStatus(
                'Active source: remote URL via dashboard fetch. Paste an image URL to continue.',
                'warning'
            );
        }

        function updateAudioSourceStatus() {
            if (!isTranscriptionRequest()) {
                return;
            }

            const sourceType = getSelectedAudioSourceType();
            const selectedFile = getSelectedAudioFile();
            if (sourceType === 'file' && selectedFile) {
                setAudioSourceStatus(
                    'Active source: uploaded audio file "' + selectedFile.name + '" (' + formatBytes(selectedFile.size) + ').',
                    'info'
                );
                return;
            }

            const audioUrl = getUrlInputValue();
            if (sourceType === 'url' && audioUrl !== '') {
                setAudioSourceStatus(
                    'Active source: remote URL via dashboard fetch. Click Preview or Run to fetch it.',
                    'warning'
                );
                return;
            }

            if (sourceType === 'file') {
                setAudioSourceStatus(
                    'Active source: uploaded audio file. Select a local audio file to continue.',
                    'info'
                );
                return;
            }

            setAudioSourceStatus(
                'Active source: remote URL via dashboard fetch. Paste an audio URL to continue.',
                'warning'
            );
        }

        function setImagePreviewSource(source) {
            const preview = getElement('preview');
            if (!preview) {
                return;
            }

            preview.onload = function () {
                preview.style.display = 'block';
                preview.onload = null;
                preview.onerror = null;
            };
            preview.onerror = function () {
                preview.style.display = 'none';
                preview.onload = null;
                preview.onerror = null;
                setImageSourceStatus('Preview failed. You can still upload the file directly.', 'warning');
            };
            preview.src = source;
        }

        function setAudioPreviewSource(source) {
            const audioPreview = getElement('audioPreview');
            if (!audioPreview) {
                return;
            }

            audioPreview.src = source;
            audioPreview.style.display = 'block';
            audioPreview.load();
        }

        function notifyStart() {
            inferenceInFlight = true;
            firstOutputNotified = false;
            if (typeof context.onRequestStart === 'function') {
                context.onRequestStart();
            }
        }

        function notifyFinish() {
            inferenceInFlight = false;
            if (typeof context.onRequestFinish === 'function') {
                context.onRequestFinish();
            }
        }

        function notifyFirstOutput() {
            if (firstOutputNotified) {
                return;
            }
            firstOutputNotified = true;
            if (typeof context.onFirstOutput === 'function') {
                context.onFirstOutput();
            }
        }

        function notifySuccess() {
            if (typeof context.onRequestSuccess === 'function') {
                context.onRequestSuccess();
            }
        }

        function notifyError(message) {
            if (typeof context.onRequestError === 'function') {
                context.onRequestError(String(message || 'Request failed.'));
            }
        }

        async function loadImage() {
            const preview = getElement('preview');
            if (!preview) {
                return;
            }

            try {
                clearImagePreview();
                syncImageSourceControls();

                const sourceType = getSelectedImageSourceType();
                if (sourceType === 'file') {
                    const selectedFile = getSelectedImageFile();
                    if (selectedFile) {
                        previewObjectUrl = URL.createObjectURL(selectedFile);
                        setImagePreviewSource(previewObjectUrl);
                        updateImageSourceStatus();
                        return;
                    }

                    updateImageSourceStatus();
                    return;
                }

                const imageUrl = getUrlInputValue();
                if (imageUrl === '') {
                    preview.style.display = 'none';
                    preview.removeAttribute('src');
                    updateImageSourceStatus();
                    return;
                }

                setImageSourceStatus(
                    'Fetching the remote image through dashboard for preview.',
                    'warning'
                );
                const fetchedBlob = await fetchRemoteImageBlob(buildRemoteImageFetchUrl(), imageUrl);
                previewObjectUrl = URL.createObjectURL(fetchedBlob);
                setImagePreviewSource(previewObjectUrl);
                updateImageSourceStatus();
            } catch (error) {
                preview.style.display = 'none';
                preview.removeAttribute('src');
                setImageSourceStatus(String(error && error.message || error), 'error');
            }
        }

        function normalizeAudioMimeType(mimeType, fallbackName) {
            const normalizedType = String(mimeType || '').split(';', 1)[0].trim().toLowerCase();
            if (AUDIO_ALLOWED_MIME_TYPES.has(normalizedType)) {
                return normalizedType;
            }

            const normalizedName = String(fallbackName || '').trim().toLowerCase();
            for (const [allowedType, extension] of Object.entries(AUDIO_MIME_TYPE_EXTENSIONS)) {
                if (normalizedName.endsWith(extension)) {
                    return allowedType;
                }
            }
            return '';
        }

        function inferAudioFilename(remoteUrl, mimeType) {
            try {
                const parsed = new URL(String(remoteUrl || ''), window.location.origin);
                const pathname = String(parsed.pathname || '');
                const lastSegment = pathname.split('/').pop() || '';
                if (lastSegment !== '' && /\.[A-Za-z0-9]+$/.test(lastSegment)) {
                    return decodeURIComponent(lastSegment);
                }
            } catch (_error) {
                // Fall through to the mime-type based fallback.
            }

            const extension = AUDIO_MIME_TYPE_EXTENSIONS[normalizeAudioMimeType(mimeType, '')] || '.wav';
            return 'remote-audio' + extension;
        }

        async function fetchRemoteAudioBlob(remoteFetchPath, url, signal) {
            let response;
            try {
                response = await fetch(new URL(remoteFetchPath, window.location.origin).toString(), {
                    method: 'POST',
                    headers: {
                        'Accept': 'application/octet-stream',
                        'Content-Type': 'application/json',
                    },
                    body: JSON.stringify({ url: url }),
                    signal: signal,
                });
            } catch (error) {
                if (error instanceof DOMException && error.name === 'AbortError') {
                    throw error;
                }
                throw new Error(
                    'Dashboard could not fetch the remote audio. Download the audio and upload it instead.'
                );
            }

            if (!response.ok) {
                let responseMessage = '';
                try {
                    responseMessage = String(await response.text() || '').trim();
                } catch (_error) {
                    responseMessage = '';
                }
                throw new Error(
                    responseMessage || ('Dashboard failed to fetch the remote audio with HTTP ' + response.status + '.')
                );
            }

            let fetchedBlob;
            try {
                fetchedBlob = await response.blob();
            } catch (error) {
                if (error instanceof DOMException && error.name === 'AbortError') {
                    throw error;
                }
                throw new Error(
                    'Remote URL fetch could not read the full audio body. Download the audio and upload it instead.'
                );
            }
            const effectiveType = normalizeAudioMimeType(
                response.headers.get('Content-Type') || fetchedBlob.type,
                url
            );
            if (!effectiveType) {
                throw new Error(
                    'Remote URL did not return a supported audio type. Use WAV, MP3, MP4, M4A, OGG, WebM, or FLAC.'
                );
            }

            if (effectiveType === fetchedBlob.type) {
                return fetchedBlob;
            }
            return new Blob([fetchedBlob], { type: effectiveType });
        }

        async function prepareTranscriptionUpload(signal) {
            const sourceType = getSelectedAudioSourceType();
            if (sourceType === 'file') {
                const selectedFile = getSelectedAudioFile();
                if (!selectedFile) {
                    throw new Error('Select an audio file before running transcription.');
                }

                const effectiveType = normalizeAudioMimeType(selectedFile.type, selectedFile.name);
                if (!effectiveType) {
                    throw new Error('Unsupported audio type. Use WAV, MP3, MP4, M4A, OGG, WebM, or FLAC.');
                }

                setAudioSourceStatus(
                    'Using uploaded audio file "' + selectedFile.name + '" (' + formatBytes(selectedFile.size) + ').',
                    'info'
                );
                return {
                    blob: effectiveType === selectedFile.type ? selectedFile : new Blob([selectedFile], { type: effectiveType }),
                    filename: selectedFile.name || ('audio' + (AUDIO_MIME_TYPE_EXTENSIONS[effectiveType] || '.wav')),
                    sizeBytes: selectedFile.size,
                };
            }

            const audioUrl = getUrlInputValue();
            if (audioUrl === '') {
                throw new Error('Provide a remote URL before running transcription.');
            }

            setAudioSourceStatus(
                'Fetching the remote audio through dashboard.',
                'warning'
            );
            const fetchedBlob = await fetchRemoteAudioBlob(buildRemoteAudioFetchUrl(), audioUrl, signal);
            const effectiveType = normalizeAudioMimeType(fetchedBlob.type, audioUrl);
            return {
                blob: fetchedBlob,
                filename: inferAudioFilename(audioUrl, effectiveType),
                sizeBytes: fetchedBlob.size,
            };
        }

        function appendMultipartField(formData, key, value) {
            if (value == null) {
                return;
            }

            let serializedValue = value;
            if (typeof serializedValue === 'boolean') {
                serializedValue = serializedValue ? 'true' : 'false';
            } else if (typeof serializedValue === 'object') {
                serializedValue = JSON.stringify(serializedValue);
            } else {
                serializedValue = String(serializedValue);
            }

            if (serializedValue.trim() === '') {
                return;
            }
            formData.append(String(key || ''), serializedValue);
        }

        async function loadAudio() {
            const urlInput = getElement('urlInput');
            const audioPreview = getElement('audioPreview');

            if (!audioPreview) {
                return;
            }

            if (isTranscriptionRequest()) {
                try {
                    clearAudioPreview();
                    syncAudioSourceControls();

                    const sourceType = getSelectedAudioSourceType();
                    if (sourceType === 'file') {
                        const selectedFile = getSelectedAudioFile();
                        if (selectedFile) {
                            audioPreviewObjectUrl = URL.createObjectURL(selectedFile);
                            setAudioPreviewSource(audioPreviewObjectUrl);
                            updateAudioSourceStatus();
                            return;
                        }

                        updateAudioSourceStatus();
                        return;
                    }

                    const url = getUrlInputValue();
                    if (url === '') {
                        updateAudioSourceStatus();
                        return;
                    }

                    setAudioSourceStatus(
                        'Fetching the remote audio through dashboard for preview.',
                        'warning'
                    );
                    const fetchedBlob = await fetchRemoteAudioBlob(buildRemoteAudioFetchUrl(), url);
                    audioPreviewObjectUrl = URL.createObjectURL(fetchedBlob);
                    setAudioPreviewSource(audioPreviewObjectUrl);
                    updateAudioSourceStatus();
                } catch (error) {
                    clearAudioPreview();
                    setAudioSourceStatus(String(error && error.message || error), 'error');
                }
                return;
            }

            const url = String(urlInput && urlInput.value || '').trim();
            if (url.endsWith('.wav') || url.endsWith('.mp4') || url.endsWith('.mp3')) {
                audioPreview.src = url;
                audioPreview.style.display = 'block';
                audioPreview.load();
                return;
            }

            window.alert('Please enter a valid audio URL ending with .wav or .mp4 or .mp3');
            audioPreview.style.display = 'none';
        }

        function buildFunccallUrl(overrides) {
            if (overrides && overrides.requestUrl) {
                return String(overrides.requestUrl);
            }
            const sampleQueryPath = (context.apiType === 'text2text' || context.apiType === 'knowledgebase')
                ? 'v1/chat/completions'
                : context.sampleQueryPath;
            return buildUrlFromSegments(context.proxyBasePath, [
                'funccall',
                context.tenant,
                context.namespace,
                context.name,
                sampleQueryPath,
            ]);
        }

        function applyPromptToRequestMap(requestMap, promptText) {
            const nextMap = normalizeOpenAIRequestMap(requestMap);

            if (Array.isArray(nextMap.messages)) {
                const patched = buildText2TextChatRequestMap(nextMap, promptText);
                if (typeof patched === 'object' && patched) {
                    return patched;
                }
            }

            if (typeof nextMap.prompt === 'string' || !Object.prototype.hasOwnProperty.call(nextMap, 'input')) {
                nextMap.prompt = promptText;
            }
            if (typeof nextMap.input === 'string') {
                nextMap.input = promptText;
            }
            return nextMap;
        }

        function buildText2TextChatRequestMap(requestMap, prompt) {
            const chatRequest = normalizeOpenAIRequestMap(requestMap);
            const promptText = String(prompt || '');
            delete chatRequest.prompt;

            const messages = Array.isArray(chatRequest.messages) ? cloneMapValue(chatRequest.messages) : [];
            if (messages.length === 0) {
                chatRequest.messages = [
                    {
                        role: 'user',
                        content: promptText,
                    },
                ];
                chatRequest.stream = true;
                return chatRequest;
            }

            let replaced = false;
            for (let i = 0; i < messages.length; i += 1) {
                const message = messages[i];
                if (!message || typeof message !== 'object') {
                    continue;
                }
                const role = String(message.role || '').trim().toLowerCase();
                if (role !== 'user') {
                    continue;
                }

                const content = message.content;
                if (typeof content === 'string') {
                    message.content = promptText;
                    replaced = true;
                    break;
                }

                if (Array.isArray(content)) {
                    const newContent = [];
                    let textReplaced = false;
                    content.forEach(function (item) {
                        if (
                            !textReplaced
                            && item
                            && typeof item === 'object'
                            && String(item.type || '').trim().toLowerCase() === 'text'
                        ) {
                            const updatedItem = cloneMapValue(item) || {};
                            updatedItem.text = promptText;
                            newContent.push(updatedItem);
                            textReplaced = true;
                            return;
                        }
                        newContent.push(cloneMapValue(item));
                    });
                    if (!textReplaced) {
                        newContent.unshift({ type: 'text', text: promptText });
                    }
                    message.content = newContent;
                    replaced = true;
                    break;
                }
            }

            if (!replaced) {
                messages.unshift({
                    role: 'user',
                    content: promptText,
                });
            }
            chatRequest.messages = messages;
            chatRequest.stream = true;
            return chatRequest;
        }

        function resolveRequestMap(overrides) {
            let requestMap = normalizeOpenAIRequestMap(context.map);
            if (overrides && typeof overrides.mapTransform === 'function') {
                const transformed = overrides.mapTransform(cloneMapValue(requestMap));
                if (transformed && typeof transformed === 'object') {
                    requestMap = normalizeOpenAIRequestMap(transformed);
                }
            }
            return requestMap;
        }

        function updateLatencyDisplays(response) {
            const startDiv = getElement('startDiv');
            const ttftDiv = getElement('ttftDiv');
            const tpsDiv = getElement('tpsDiv');
            const restore = response.headers.get('X-TcpConn-Latency');
            const ttft = response.headers.get('X-Ttft-Latency');

            if (startDiv) {
                startDiv.innerHTML = 'Start Latency: ' + formatLatencyValue(restore) + ' ms <br>';
            }
            if (ttftDiv) {
                ttftDiv.innerHTML = 'Time To First Token: ' + formatLatencyValue(ttft) + ' ms <br>';
            }
            if (tpsDiv && !response.ok) {
                tpsDiv.textContent = '';
            }
        }

        async function prepareImageData(signal) {
            const sourceType = getSelectedImageSourceType();
            if (sourceType === 'file') {
                const selectedFile = getSelectedImageFile();
                if (!selectedFile) {
                    throw new Error('Select an image file before running image2text.');
                }

                const normalizedUpload = await normalizeImageBlob(
                    selectedFile,
                    selectedFile.name || selectedFile.type
                );
                setImageSourceStatus(
                    'Using uploaded file. Sending ' + formatBytes(normalizedUpload.sizeBytes) + ' at '
                        + normalizedUpload.width + 'x' + normalizedUpload.height + '.',
                    normalizedUpload.sizeBytes > IMAGE_TARGET_DATA_URL_BYTES ? 'warning' : 'info'
                );
                return normalizedUpload;
            }

            const imageUrl = getUrlInputValue();
            if (imageUrl === '') {
                throw new Error('Provide a remote URL before running image2text.');
            }

            setImageSourceStatus(
                'Fetching the remote image through dashboard.',
                'warning'
            );
            const fetchedBlob = await fetchRemoteImageBlob(buildRemoteImageFetchUrl(), imageUrl, signal);
            const normalizedUrlUpload = await normalizeImageBlob(fetchedBlob, imageUrl);
            setImageSourceStatus(
                'Using remote URL via dashboard fetch. Sending ' + formatBytes(normalizedUrlUpload.sizeBytes) + ' at '
                    + normalizedUrlUpload.width + 'x' + normalizedUrlUpload.height + '.',
                normalizedUrlUpload.sizeBytes > IMAGE_TARGET_DATA_URL_BYTES ? 'warning' : 'info'
            );
            return normalizedUrlUpload;
        }

        function resetVisualOutputs() {
            const output = getElement('output');
            const debug = getElement('debug');
            const tpsDiv = getElement('tpsDiv');
            const myImage = getElement('myImage');
            const myAudio = getElement('myAudio');

            setElementText(output, '');
            if (debug && typeof debug.value === 'string') {
                debug.value = '';
            }
            if (tpsDiv) {
                tpsDiv.textContent = '';
            }
            if (output) {
                output.hidden = false;
            }
            if (myImage) {
                myImage.style.display = 'none';
                myImage.removeAttribute('src');
            }
            if (myAudio) {
                myAudio.pause();
                myAudio.style.display = 'none';
                myAudio.removeAttribute('src');
            }
        }

        function startRequestUi() {
            const button = getElement('button');
            const cancelBtn = getElement('cancel');
            const processing = getElement('processing');
            const runLabel = getElement('run-label');

            abortController = new AbortController();
            if (cancelBtn) {
                cancelBtn.disabled = false;
            }
            setBusyState(button, processing, runLabel, true);
            notifyStart();
            return abortController.signal;
        }

        function finishRequestUi() {
            const button = getElement('button');
            const cancelBtn = getElement('cancel');
            const processing = getElement('processing');
            const runLabel = getElement('run-label');

            abortController = null;
            if (cancelBtn) {
                cancelBtn.disabled = true;
            }
            setBusyState(button, processing, runLabel, false);
            notifyFinish();
        }

        async function streamOutputText(overrides) {
            const output = getElement('output');
            const debug = getElement('debug');
            const prompt = getPromptValue();
            const inputUrl = String((getElement('urlInput') || {}).value || '');
            const tpsDiv = getElement('tpsDiv');
            const signal = startRequestUi();

            resetVisualOutputs();

            try {
                const requestMap = resolveRequestMap(overrides);
                let body = '';
                let requestHeaders = {
                    'Accept': 'application/json',
                };
                let timeoutPayloadBytes = 0;
                const isTranscription = isTranscriptionRequest();

                if (context.apiType === 'text2text' || context.apiType === 'knowledgebase') {
                    const normalizedPath = String(context.sampleQueryPath || '')
                        .trim()
                        .toLowerCase()
                        .replace(/^\/+/, '');
                    const useChatShape = context.apiType === 'text2text'
                        || context.apiType === 'knowledgebase'
                        || normalizedPath === 'v1/chat/completions'
                        || Array.isArray(requestMap.messages);
                    body = JSON.stringify(
                        useChatShape
                            ? buildText2TextChatRequestMap(requestMap, prompt)
                            : applyPromptToRequestMap(requestMap, prompt)
                    );
                    requestHeaders['Content-Type'] = 'application/json';
                } else if (context.apiType === 'image2text') {
                    const imageData = await prepareImageData(signal);
                    body = JSON.stringify({
                        model: requestMap.model,
                        messages: [
                            {
                                role: 'user',
                                content: [
                                    { type: 'text', text: prompt },
                                    {
                                        type: 'image_url',
                                        image_url: {
                                            url: imageData.dataUrl,
                                        },
                                    },
                                ],
                            },
                        ],
                        max_tokens: requestMap.max_tokens,
                        temperature: requestMap.temperature,
                        stream: true,
                    });
                    requestHeaders['Content-Type'] = 'application/json';
                    timeoutPayloadBytes = new Blob([body]).size;
                } else if (isTranscription) {
                    const audioUpload = await prepareTranscriptionUpload(signal);
                    const formData = new FormData();
                    formData.append('file', audioUpload.blob, audioUpload.filename);
                    const transcriptionPrompt = getEffectiveTranscriptionPrompt();
                    if (transcriptionPrompt !== '') {
                        formData.append('prompt', transcriptionPrompt);
                    }
                    Object.entries(requestMap).forEach(function ([key, value]) {
                        if (key === 'model' || key === 'prompt' || key === 'messages' || key === 'stream') {
                            return;
                        }
                        appendMultipartField(formData, key, value);
                    });
                    body = formData;
                    timeoutPayloadBytes = audioUpload.sizeBytes;
                } else {
                    body = JSON.stringify({
                        model: requestMap.model,
                        messages: [
                            {
                                role: 'user',
                                content: [
                                    { type: 'text', text: prompt },
                                    {
                                        type: 'audio_url',
                                        audio_url: {
                                            url: inputUrl,
                                        },
                                    },
                                ],
                            },
                        ],
                        max_tokens: requestMap.max_tokens,
                        temperature: requestMap.temperature,
                        stream: true,
                    });
                    requestHeaders['Content-Type'] = 'application/json';
                }

                requestHeaders = mergeHeaders(requestHeaders, overrides && overrides.extraHeaders);

                appendElementText(debug, JSON.stringify(requestMap, null, 2));
                if (isTranscription) {
                    appendElementText(debug, '\n' + JSON.stringify({
                        transcription_path: context.sampleQueryPath,
                        source: getSelectedAudioSourceType(),
                    }, null, 2));
                }

                requestHeaders['X-Inferx-Timeout'] = computeRequestTimeoutSeconds(
                    isTranscription ? 'transcriptions' : context.apiType,
                    timeoutPayloadBytes
                );

                const response = await fetch(buildFunccallUrl(overrides), {
                    method: 'POST',
                    headers: requestHeaders,
                    body: body,
                    signal: signal,
                });

                updateLatencyDisplays(response);

                if (!response.ok) {
                    const errorText = await response.text();
                    setElementText(output, errorText);
                    notifyError(errorText || ('HTTP ' + response.status));
                    return;
                }

                if (isTranscription) {
                    const responseText = String(await response.text() || '');
                    try {
                        const parsed = JSON.parse(responseText);
                        if (
                            parsed
                            && typeof parsed === 'object'
                            && !Array.isArray(parsed)
                            && typeof parsed.text === 'string'
                            && Object.keys(parsed).length === 1
                        ) {
                            setElementText(output, parsed.text);
                        } else {
                            setElementText(output, JSON.stringify(parsed, null, 2));
                        }
                    } catch (_error) {
                        setElementText(output, responseText);
                    }
                    notifyFirstOutput();
                    notifySuccess();
                    return;
                }

                if (!response.body) {
                    return;
                }

                const reader = response.body.getReader();
                const decoder = new TextDecoder('utf-8');
                const startTime = Date.now();
                let tokenCount = 0;
                let buffer = '';

                while (true) {
                    const result = await reader.read();
                    if (result.done) {
                        break;
                    }

                    buffer += decoder.decode(result.value, { stream: true });
                    const lines = buffer.split('\n');
                    buffer = lines.pop() || '';

                    for (const line of lines) {
                        const trimmed = line.trim();
                        if (trimmed === '' || !trimmed.startsWith('data:')) {
                            continue;
                        }

                        const jsonPart = trimmed.replace(/^data:\s*/, '');
                        if (jsonPart === '[DONE]') {
                            return;
                        }

                        try {
                            const parsed = JSON.parse(jsonPart);
                            const content = parsed.choices?.[0]?.delta?.content ?? parsed.choices?.[0]?.text ?? '';
                            if (content) {
                                notifySuccess();
                                notifyFirstOutput();
                                appendElementText(output, content);
                                tokenCount += 1;
                                const elapsed = Math.max((Date.now() - startTime) / 1000, 0.001);
                                if (tpsDiv) {
                                    tpsDiv.textContent = 'TPS: ' + (tokenCount / elapsed).toFixed(0) + '     Tokens: ' + tokenCount;
                                }
                            }
                        } catch (error) {
                            console.warn('JSON parse error:', error, 'on line:', jsonPart);
                            buffer = jsonPart + buffer;
                        }
                    }
                }
            } catch (error) {
                if (!(error instanceof DOMException && error.name === 'AbortError')) {
                    console.error('Error fetching inference output:', error);
                    if (context.apiType === 'image2text') {
                        setImageSourceStatus(String(error && error.message || error), 'error');
                    }
                    notifyError(String(error && error.message || error));
                    setElementText(output, String(error && error.message || error));
                }
            } finally {
                finishRequestUi();
            }
        }

        async function streamOutputImage(overrides) {
            const output = getElement('output');
            const prompt = String((getElement('prompt') || {}).value || '');
            const signal = startRequestUi();

            resetVisualOutputs();

            try {
                let requestUrl = new URL(context.text2imgPath, window.location.origin).toString();
                let requestHeaders = {
                    'Accept': 'application/json',
                    'Content-Type': 'application/json',
                    'X-Inferx-Timeout': '60',
                };
                const requestMap = resolveRequestMap(overrides);
                let requestBody = JSON.stringify({
                    prompt: prompt,
                    tenant: context.tenant,
                    namespace: context.namespace,
                    funcname: context.name,
                });

                if (overrides && overrides.requestUrl) {
                    requestUrl = buildFunccallUrl(overrides);
                    requestHeaders = mergeHeaders(requestHeaders, overrides.extraHeaders);
                    requestBody = JSON.stringify(applyPromptToRequestMap(requestMap, prompt));
                }

                const response = await fetch(requestUrl, {
                    method: 'POST',
                    headers: requestHeaders,
                    body: requestBody,
                    signal: signal,
                });

                updateLatencyDisplays(response);

                if (!response.ok) {
                    const errorText = await response.text();
                    setElementText(output, errorText);
                    notifyError(errorText || ('HTTP ' + response.status));
                    return;
                }

                const data = await response.json();
                const base64Data = data?.choices?.[0]?.message?.content?.[0]?.image_url?.url;
                if (!base64Data) {
                    throw new Error('Image response was missing image data');
                }

                const image = getElement('myImage');
                if (image) {
                    image.src = base64Data;
                    image.style.display = 'block';
                }
                notifySuccess();
                notifyFirstOutput();
                if (output) {
                    output.hidden = true;
                }
            } catch (error) {
                if (!(error instanceof DOMException && error.name === 'AbortError')) {
                    console.error('Error fetching image output:', error);
                    notifyError(String(error && error.message || error));
                    setElementText(output, String(error && error.message || error));
                }
            } finally {
                finishRequestUi();
            }
        }

        async function streamOutputAudio(overrides) {
            const output = getElement('output');
            const prompt = String((getElement('prompt') || {}).value || '');
            const signal = startRequestUi();
            resetVisualOutputs();

            // --- CONFIGURATION ---
            const SOURCE_RATE = 24000; // Updated to your target rate
            const BUFFER_THRESHOLD = 0.03; // 150ms "safety" buffer to prevent stuttering
            // ---------------------

            let leftOverByte = null;
            // Create and resume AudioContext before fetch (user gesture)
            const audioCtx = new (window.AudioContext || window.webkitAudioContext)();
            await audioCtx.resume();
            let nextTime = audioCtx.currentTime;  // immediate playback
            let isStarted = false

            try {
                const t0 = performance.now();

                let requestUrl = new URL(context.text2audioPath, window.location.origin).toString();
                let requestHeaders = { 'Content-Type': 'application/json', 'X-Inferx-Timeout': '60', 'X-Accel-Buffering': 'no' };
                const requestMap = resolveRequestMap(overrides);
                let requestBody = JSON.stringify({
                    prompt: prompt,
                    tenant: context.tenant,
                    namespace: context.namespace,
                    funcname: context.name,
                    stream: true,
                    response_format: "pcm",
                    voice: 'Vivian',
                });

                if (overrides && overrides.requestUrl) {
                    requestUrl = buildFunccallUrl(overrides);
                    requestHeaders = mergeHeaders(requestHeaders, overrides.extraHeaders);
                    requestBody = JSON.stringify(applyPromptToRequestMap(requestMap, prompt));
                }

                const response = await fetch(requestUrl, {
                    method: 'POST',
                    headers: requestHeaders,
                    body: requestBody,
                    signal: signal,
                    priority: 'high',
                    cache: 'no-store'
                });
                updateLatencyDisplays(response);

                if (!response.ok || !response.body) {
                    const errorText = await response.text();
                    setElementText(output, errorText);
                    notifyError(errorText || ('HTTP ' + response.status));
                    return;
                }

                const reader = response.body.getReader();
                notifyFirstOutput();

                while (true) {
                    const { value, done } = await reader.read();
                    if (done) break;

                    // Initialize Context on first chunk (browser requirement for user gesture)
                    if (!audioCtx) {
                        audioCtx = new (window.AudioContext || window.webkitAudioContext)();
                        nextTime = audioCtx.currentTime + 0.02;
                    }

                    let chunk = value;

                    // Handle Byte Alignment for PCM16 (2 bytes per sample)
                    if (leftOverByte !== null) {
                        const combined = new Uint8Array(chunk.length + 1);
                        combined[0] = leftOverByte;
                        combined.set(chunk, 1);
                        chunk = combined;
                        leftOverByte = null;
                    }

                    if (chunk.byteLength % 2 !== 0) {
                        leftOverByte = chunk[chunk.length - 1];
                        chunk = chunk.slice(0, chunk.length - 1);
                    }

                    if (chunk.byteLength <= 0) continue;

                    // Convert PCM16 to Float32
                    const pcm16 = new Int16Array(chunk.buffer, chunk.byteOffset, chunk.byteLength / 2);
                    const float32 = new Float32Array(pcm16.length);
                    for (let i = 0; i < pcm16.length; i++) {
                        float32[i] = pcm16[i] / 32768.0;
                    }

                    // Resample
                    const resampled = resampleLinear(float32, SOURCE_RATE, audioCtx.sampleRate);

                    // Create and Schedule Buffer
                    const audioBuffer = audioCtx.createBuffer(1, resampled.length, audioCtx.sampleRate);
                    audioBuffer.copyToChannel(resampled, 0);

                    const source = audioCtx.createBufferSource();
                    source.buffer = audioBuffer;
                    source.connect(audioCtx.destination);

                    // JITTER MANAGEMENT:
                    // If the network fell behind and we are already past 'nextTime', 
                    // reset nextTime to slightly in the future to rebuild the buffer.
                    if (nextTime < audioCtx.currentTime) {
                        nextTime = audioCtx.currentTime + BUFFER_THRESHOLD;
                    }

                    source.start(nextTime);
                    nextTime += audioBuffer.duration;
                }
                notifySuccess();
            } catch (error) {
                if (!(error instanceof DOMException && error.name === 'AbortError')) {
                    console.error('Streaming Error:', error);
                    notifyError(String(error && error.message || error));
                    setElementText(output, String(error && error.message || error));
                }
            } finally {
                finishRequestUi();
            }
        }

        function resampleLinear(input, srcRate, dstRate) {
            if (srcRate === dstRate) return input;
            const ratio = srcRate / dstRate;
            const newLength = Math.floor(input.length / ratio);
            const output = new Float32Array(newLength);
            for (let i = 0; i < newLength; i++) {
                const pos = i * ratio;
                const idx = Math.floor(pos);
                const frac = pos - idx;
                const s1 = input[idx] || 0;
                const s2 = (idx + 1 < input.length) ? input[idx + 1] : s1;
                output[i] = s1 + (s2 - s1) * frac;
            }
            return output;
        }

        function cancel() {
            if (abortController) {
                abortController.abort();
            }
        }

        async function streamOutput(overrides) {
            if (context.apiType === 'text2img') {
                await streamOutputImage(overrides);
                return;
            }
            if (context.apiType === 'text2audio') {
                await streamOutputAudio(overrides);
                return;
            }
            await streamOutputText(overrides);
        }

        if (context.apiType === 'image2text') {
            const imageFileInput = getElement('imageFileInput');
            const urlInput = getElement('urlInput');
            const imageSourceInputs = document.querySelectorAll('input[name="imageSourceType"]');

            if (imageFileInput) {
                imageFileInput.addEventListener('change', function () {
                    loadImage();
                });
            }
            if (urlInput) {
                urlInput.addEventListener('input', function () {
                    clearImagePreview();
                    updateImageSourceStatus();
                });
            }
            imageSourceInputs.forEach(function (input) {
                input.addEventListener('change', function () {
                    syncImageSourceControls();
                    if (getSelectedImageSourceType() === 'file' && getSelectedImageFile()) {
                        loadImage();
                        return;
                    }
                    clearImagePreview();
                    updateImageSourceStatus();
                });
            });

            syncImageSourceControls();
            updateImageSourceStatus();
        }

        if (isTranscriptionRequest()) {
            const audioFileInput = getElement('audioFileInput');
            const urlInput = getElement('urlInput');
            const audioSourceInputs = document.querySelectorAll('input[name="audioSourceType"]');

            if (audioFileInput) {
                audioFileInput.addEventListener('change', function () {
                    loadAudio();
                });
            }
            if (urlInput) {
                urlInput.addEventListener('input', function () {
                    clearAudioPreview();
                    updateAudioSourceStatus();
                });
            }
            audioSourceInputs.forEach(function (input) {
                input.addEventListener('change', function () {
                    syncAudioSourceControls();
                    if (getSelectedAudioSourceType() === 'file' && getSelectedAudioFile()) {
                        loadAudio();
                        return;
                    }
                    clearAudioPreview();
                    updateAudioSourceStatus();
                });
            });

            syncAudioSourceControls();
            updateAudioSourceStatus();
        }

        return {
            loadImage: loadImage,
            loadAudio: loadAudio,
            streamOutput: streamOutput,
            streamOutputWithOverrides: streamOutput,
            cancel: cancel,
            isInferenceInFlight: function () {
                return inferenceInFlight;
            },
        };
    }

    window.createInferxInferenceController = createInferenceController;
}());
