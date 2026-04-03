(function () {
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

    function formatLatencyValue(value) {
        const text = String(value || '').trim();
        return text === '' ? '-' : text;
    }

    function createInferenceController(config) {
        const context = config || {};
        let abortController = null;
        let inferenceInFlight = false;
        let firstOutputNotified = false;

        function getElement(id) {
            return document.getElementById(id);
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

        function loadImage() {
            const urlInput = getElement('urlInput');
            const preview = getElement('preview');
            const url = String(urlInput && urlInput.value || '').trim();

            if (!preview) {
                return;
            }

            if (url.endsWith('.jpg') || url.endsWith('.jpeg')) {
                preview.src = url;
                preview.style.display = 'block';
                return;
            }

            window.alert('Please enter a valid JPEG image URL ending with .jpg or .jpeg');
            preview.style.display = 'none';
        }

        function loadAudio() {
            const urlInput = getElement('urlInput');
            const audioPreview = getElement('audioPreview');
            const url = String(urlInput && urlInput.value || '').trim();

            if (!audioPreview) {
                return;
            }

            if (url.endsWith('.wav') || url.endsWith('.mp4') || url.endsWith('.mp3')) {
                audioPreview.src = url;
                audioPreview.style.display = 'block';
                audioPreview.load();
                return;
            }

            window.alert('Please enter a valid audio URL ending with .wav or .mp4 or .mp3');
            audioPreview.style.display = 'none';
        }

        function buildFunccallUrl() {
            return buildUrlFromSegments(context.proxyBasePath, [
                'funccall',
                context.tenant,
                context.namespace,
                context.name,
                context.sampleQueryPath,
            ]);
        }

        function updateLatencyDisplays(response) {
            const startDiv = getElement('startDiv');
            const ttftDiv = getElement('ttftDiv');
            const tpsDiv = getElement('tpsDiv');
            const restore = response.headers.get('tcpconn_latency_header');
            const ttft = response.headers.get('ttft_latency_header');

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

        async function streamOutputText() {
            const output = getElement('output');
            const debug = getElement('debug');
            const prompt = String((getElement('prompt') || {}).value || '');
            const inputUrl = String((getElement('urlInput') || {}).value || '');
            const tpsDiv = getElement('tpsDiv');
            const signal = startRequestUi();

            resetVisualOutputs();

            try {
                const requestMap = cloneMapValue(context.map) || {};
                let body = '';

                if (context.apiType === 'text2text') {
                    requestMap.prompt = prompt;
                    body = JSON.stringify(requestMap);
                } else if (context.apiType === 'image2text') {
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
                }

                appendElementText(debug, JSON.stringify(requestMap, null, 2));

                const response = await fetch(buildFunccallUrl(), {
                    method: 'POST',
                    headers: {
                        'Accept': 'application/json',
                        'Content-Type': 'application/json',
                        'X-Inferx-Timeout': '60',
                    },
                    body: body,
                    signal: signal,
                });

                updateLatencyDisplays(response);

                if (!response.ok) {
                    setElementText(output, await response.text());
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
                }
            } finally {
                finishRequestUi();
            }
        }

        async function streamOutputImage() {
            const output = getElement('output');
            const prompt = String((getElement('prompt') || {}).value || '');
            const signal = startRequestUi();

            resetVisualOutputs();

            try {
                const response = await fetch(new URL(context.text2imgPath, window.location.origin).toString(), {
                    method: 'POST',
                    headers: {
                        'Accept': 'application/json',
                        'Content-Type': 'application/json',
                        'X-Inferx-Timeout': '60',
                    },
                    body: JSON.stringify({
                        prompt: prompt,
                        tenant: context.tenant,
                        namespace: context.namespace,
                        funcname: context.name,
                    }),
                    signal: signal,
                });

                updateLatencyDisplays(response);

                if (!response.ok) {
                    setElementText(output, await response.text());
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
                notifyFirstOutput();
                if (output) {
                    output.hidden = true;
                }
            } catch (error) {
                if (!(error instanceof DOMException && error.name === 'AbortError')) {
                    console.error('Error fetching image output:', error);
                    setElementText(output, String(error && error.message || error));
                }
            } finally {
                finishRequestUi();
            }
        }

        async function streamOutputAudio() {
            const output = getElement('output');
            const prompt = String((getElement('prompt') || {}).value || '');
            const signal = startRequestUi();

            resetVisualOutputs();

            try {
                const response = await fetch(new URL(context.text2audioPath, window.location.origin).toString(), {
                    method: 'POST',
                    headers: {
                        'Accept': 'application/json',
                        'Content-Type': 'application/json',
                        'X-Inferx-Timeout': '60',
                    },
                    body: JSON.stringify({
                        prompt: prompt,
                        tenant: context.tenant,
                        namespace: context.namespace,
                        funcname: context.name,
                    }),
                    signal: signal,
                });

                updateLatencyDisplays(response);

                if (!response.ok) {
                    setElementText(output, await response.text());
                    return;
                }

                const audioBlob = await response.blob();
                const audioUrl = URL.createObjectURL(audioBlob);
                const audio = getElement('myAudio');
                if (audio) {
                    audio.src = audioUrl;
                    audio.style.display = 'block';
                    audio.play().catch(() => {});
                }
                notifyFirstOutput();
                if (output) {
                    output.hidden = true;
                }
            } catch (error) {
                if (!(error instanceof DOMException && error.name === 'AbortError')) {
                    console.error('Error fetching audio output:', error);
                    setElementText(output, String(error && error.message || error));
                }
            } finally {
                finishRequestUi();
            }
        }

        function cancel() {
            if (abortController) {
                abortController.abort();
            }
        }

        async function streamOutput() {
            if (context.apiType === 'text2img') {
                await streamOutputImage();
                return;
            }
            if (context.apiType === 'text2audio') {
                await streamOutputAudio();
                return;
            }
            await streamOutputText();
        }

        return {
            loadImage: loadImage,
            loadAudio: loadAudio,
            streamOutput: streamOutput,
            cancel: cancel,
            isInferenceInFlight: function () {
                return inferenceInFlight;
            },
        };
    }

    window.createInferxInferenceController = createInferenceController;
}());
