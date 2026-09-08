# Free local models for AI Call Assistant
Research date: September 8, 2026

**Recommendation:** use **Ollama + Qwen3.5 2B** as the first local answer-generation experiment on this laptop. Compare **Qwen3.5 4B** and **Phi-4-mini** for answer quality. For speech, start with **whisper.cpp + base.en** for recorded-file testing, then evaluate **Moonshine Tiny/Small Streaming** for live transcripts.

**Yes, API-fee-free local testing is feasible. The app needs integration work before its Record and Ask buttons can use these models.** Its existing automated tests already use fakes and local scripted servers without paid model calls. Keep those tests; add real-model evaluations as a separate, optional test layer.

This is a source-backed feasibility report, not a model benchmark. Hardware and repository code were inspected. No models were installed, downloaded, or run, and no local-provider integration was implemented. Rankings below reflect suitability for this app and computer, not a measured quality or speed leaderboard.

## 1. Hardware and practical limits

The local Windows hardware query reported:

| Component | Observed |
| --- | --- |
| CPU | Intel Core i7-8665U, 4 physical cores |
| System memory | 15.8 GiB, approximately 16 GB |
| Graphics | Intel UHD Graphics 620 integrated graphics |
| Local runtimes | Neither `ollama` nor `lms` was found on the current PATH |

Treat this as a **CPU-first test machine**. No dedicated NVIDIA/AMD adapter was detected. The Windows graphics query's memory figure is not a dependable measure of dedicated model VRAM for an integrated GPU.

My assessment: small quantized models are reasonable for functional tests and interview practice. Do not assume this laptop can sustain the app's roughly one-second stop-to-first-word goal, particularly while transcription, a call, and other development processes share the CPU. This must be measured.

Download size is **not** runtime RAM usage: model execution also needs context storage, working buffers, and application memory. Use one loaded answer model at a time, start with a 4,096-token context, and measure memory pressure before increasing it. These are proposed test settings, not guaranteed requirements or speed estimates.

## 2. Best answer-model candidates

The download sizes below are the default Ollama artifacts observed on the research date; tags and packaging can change. Record the exact model digest and quantization during testing.

| Candidate | Download | Best use here | Recommendation |
| --- | ---: | --- | --- |
| **Qwen3.5 2B** — `qwen3.5:2b` | 2.7 GB | First useful local answer experiment | **Start here.** Compare concise answers and resume grounding with thinking disabled. |
| **Qwen3.5 4B** — `qwen3.5:4b` | 3.4 GB | Quality comparison on this laptop | Try second; retain it only if better answers justify measured latency. |
| **Phi-4-mini** — `phi4-mini:3.8b` | 2.5 GB | Alternative model family and technical questions | Useful independent comparison; use the instruct model, not a similarly named reasoning variant. |
| **Qwen3.5 0.8B** — `qwen3.5:0.8b` | 1.0 GB | Lightweight streaming, cancellation, and error-path smoke tests | Use when resource usage matters most; do not treat it as the answer-quality reference. |
| **Qwen3.5 9B** — `qwen3.5:9b` | 6.6 GB | Optional higher-capacity comparison | Defer on this laptop. It may fit RAM, but fitting does not establish acceptable speed. |

Artifact facts: [Ollama Qwen3.5 library](https://ollama.com/library/qwen3.5), [Ollama Phi-4-mini library](https://ollama.com/library/phi4-mini). The 2B and 4B Qwen model cards identify Apache-2.0 licenses; Phi-4-mini's card identifies MIT. [Qwen 2B card](https://huggingface.co/Qwen/Qwen3.5-2B), [Qwen 4B card](https://huggingface.co/Qwen/Qwen3.5-4B), [Microsoft model card](https://huggingface.co/microsoft/Phi-4-mini-instruct).

**Why compare 4B with 2B?** This is a resource/quality experiment, not a claim that 4B always wins. Qwen publishes size-specific evaluations, but those do not establish performance on this app's resume-grounded interview task or on quantized CPU inference. [Qwen 4B evaluation card](https://huggingface.co/Qwen/Qwen3.5-4B).

**Disable thinking for latency tests.** Qwen3.5 defaults to thinking in its model documentation. Ollama exposes a `think` control and separates thinking output from final-answer content. Hiding the trace is different from disabling the extra computation. Measure the first actual answer text, not the first reasoning token. [Qwen card](https://huggingface.co/Qwen/Qwen3.5-4B), [Ollama thinking controls](https://docs.ollama.com/capabilities/thinking).

**Why not start with Gemma 4?** It is a valid Apache-2.0 alternative, but the observed Ollama E2B and E4B downloads are 7.2 GB and 9.6 GB. “Effective” parameter labels do not mean comparably tiny downloads. Given this machine, smaller candidates are a more economical first experiment. [Gemma artifacts](https://ollama.com/library/gemma4), [Google model card](https://ai.google.dev/gemma/docs/core/model_card_4).

## 3. Runtime choice

| Runtime | Role in this project | Tradeoff |
| --- | --- | --- |
| **Ollama** | Recommended first local service; native Windows app, model management, localhost API | Easy to script and swap models. Keep local-only configuration explicit. |
| **LM Studio** | Best GUI alternative for manually comparing models and prompts | Windows support and a local compatible API; free at home and work. Its published Windows recommendations include 16 GB RAM and 4 GB dedicated VRAM, so benchmark CPU use on this laptop. |
| **llama.cpp** | Best later option when precise inference control or a packaged native helper matters | C/C++, quantization, CPU backends, and a compatible server; more packaging and lifecycle work for us. |

Sources: [Ollama Windows setup](https://docs.ollama.com/windows), [Ollama project](https://github.com/ollama/ollama), [LM Studio server documentation](https://lmstudio.ai/docs/developer), [LM Studio free-at-work announcement](https://beta.lmstudio.ai/blog/free-for-work), [LM Studio requirements](https://lmstudio.ai/docs/app/system-requirements), [llama.cpp project](https://github.com/ggml-org/llama.cpp).

“Free” here means no per-request hosted inference charge when the model runs locally. Downloads, disk space, electricity, and engineering time still have costs. Runtime licensing and model licensing are separate; LM Studio being free to use does not make every downloadable model identically licensed.

## 4. Best local speech options

| Model/runtime | Recommended test | Fit and limitation |
| --- | --- | --- |
| **Whisper base.en + whisper.cpp** | First English recorded-file transcription baseline | CPU-only execution and Windows support; established C/C++ integration path. The project's base-model table lists 142 MiB on disk and about 388 MB memory. |
| **Whisper tiny.en + whisper.cpp** | Lighter smoke tests | The tiny table lists 75 MiB and about 273 MB. Compare recognition errors before choosing it over base.en. |
| **Moonshine Tiny Streaming / Small Streaming** | First candidates for replacing live Deepgram transcripts | English versions have 34M/123M parameters and MIT licenses. Streaming work happens while speech arrives, which matches this app's latency needs. |
| **faster-whisper, CPU int8** | Python benchmark harness or an experimental helper service | Convenient CPU quantization; adds Python/CTranslate2 deployment. Its documented CUDA acceleration needs NVIDIA libraries, so that route does not apply to the detected GPU. |
| **Streaming Zipformer through sherpa-onnx** | Alternative if configurable streaming infrastructure is preferred | Offline Windows support and streaming APIs. Select and record an exact language checkpoint and its own license before adoption. |

Sources: [whisper.cpp platform and memory documentation](https://github.com/ggml-org/whisper.cpp), [Moonshine project](https://github.com/moonshine-ai/moonshine), [Moonshine model list](https://moonshine-voice.readthedocs.io/en/latest/models/available-models/), [faster-whisper](https://github.com/SYSTRAN/faster-whisper), [sherpa-onnx documentation](https://k2-fsa.github.io/sherpa/onnx/index.html).

The Whisper memory figures are upstream reference values, not measurements here. Whisper is not inherently a real-time streaming recognizer; rolling windows, partial-text stabilization, and endpoint handling are extra integration work. A completed-file HTTP transcription endpoint is not a drop-in replacement for Deepgram's live WebSocket behavior. [Whisper streaming research](https://arxiv.org/abs/2307.14743).

For English, compare Moonshine Tiny Streaming first under CPU contention, then Small Streaming for accuracy. Its published WER values come from specified datasets and reference models; they should not be read as accuracy guarantees for noisy interview audio. Legacy non-English, non-streaming Moonshine checkpoints have different license terms, so choose the named streaming checkpoint deliberately. [Moonshine model details](https://moonshine-voice.readthedocs.io/en/latest/models/available-models/).

## 5. What needs to change in this app

These findings come from the current source, not assumptions about a generic chat app.

| Existing behavior | Consequence |
| --- | --- |
| Only Anthropic/Groq are in `LlmProviderKind` and settings | Installing Ollama alone does not add a selectable local provider. |
| Groq uses a fixed model, `/openai/v1/chat/completions`, and provider-specific fields | Its `with_base_url` test hook is insufficient by itself. A different base URL leaves the model, path, and request assumptions wrong. |
| Ask requires an answer-provider key; Record also requires a Deepgram key | A local mode needs provider-aware validation, not a dummy-key workaround. |
| Speech accepts 16 kHz mono i16 PCM and emits accumulated partial transcripts | Local STT must preserve that contract and keep inference off the audio callback. |
| Core deadlines are 5 s STT connect/finalize, 10 s to first answer token, and 60 s total | Cold model loading and slow CPU inference can trigger real failures. Warm-up must precede timed sessions. |
| Each resume/JD field permits 200,000 characters | A small local context can overflow. Add token-aware budgeting and visible feedback rather than silently dropping grounding text. |

Code references: [provider contract](../src-tauri/core/src/llm/mod.rs), [Groq implementation](../src-tauri/core/src/llm/groq.rs), [command validation and dependency construction](../src-tauri/src/commands.rs), [speech contract](../src-tauri/core/src/stt/mod.rs), [deadlines and metrics](../src-tauri/core/src/session/mod.rs), [profile limits](../src-tauri/core/src/store/mod.rs), [frontend settings contract](../src/types.ts).

### Recommended implementation order

1. **Local typed Ask.** Add a separate local provider with model name, loopback base URL, and inference options. Reuse the existing OpenAI-style SSE decoding for `/v1/chat/completions`, preserving cancellation and the exact concatenation of displayed deltas. Ollama and LM Studio expose compatible APIs, but test the subset we actually use. [Ollama compatibility](https://docs.ollama.com/api/openai-compatibility), [LM Studio APIs](https://lmstudio.ai/docs/developer).
2. **Provider-specific requests.** Use supported output-token and reasoning controls. Ollama's compatibility documentation lists `max_tokens` and `reasoning_effort`, including `none`; do not blindly carry Groq's `include_reasoning` or model-specific settings across. If using native `/api/chat` instead, add an NDJSON adapter rather than feeding it to the SSE parser. [Compatibility fields](https://docs.ollama.com/api/openai-compatibility), [native chat API](https://docs.ollama.com/api/chat).
3. **Readiness and context limits.** Add a connection check, explicit model-loading status, preloading, and a bounded local context. Existing origin prewarming opens a network connection; it does not guarantee model residency. Preserve cloud deadlines; expose slower experimental local settings separately if needed.
4. **Local recorded-file STT, then live STT.** Implement `SttConnector`/`SttStream`, with bounded audio queues, idempotent finalize, cancellation, and full accumulated partial text. Reuse the existing system-audio capture; do not replace it with sample applications that capture a microphone.
5. **Keep automated tests deterministic.** Extend the existing fake providers and scripted loopback servers for protocol/error tests. Put real-model checks behind a separate opt-in command. They supplement unit tests and do not establish cloud-provider equivalence. [Current test architecture](TESTING.md).

For a local provider, default to a loopback address and avoid forwarding saved cloud API keys. Prove offline operation with network access disabled after model downloads. This makes the intended local behavior testable.

## 6. Suggested first experiment

These commands are a proposed manual smoke test, **not executed during this research**. They test Ollama directly; the app will still require the changes above.

Install Ollama from its official Windows distribution, then download only the first candidate:

```powershell
ollama pull qwen3.5:2b
ollama run qwen3.5:2b --think=false "Give a concise explanation of unit testing."
ollama ps
```

Use explicit size tags rather than `latest` or `:cloud`. After downloading, disable cloud features with `OLLAMA_NO_CLOUD=1` and restart Ollama if strict local-only operation is required. [Windows installation](https://docs.ollama.com/windows), [thinking CLI](https://docs.ollama.com/capabilities/thinking), [local-only configuration](https://docs.ollama.com/faq).

A bounded direct API smoke test:

```powershell
$testBody = @{
    model = "qwen3.5:2b"
    messages = @(
        @{ role = "system"; content = "Answer briefly. Do not invent personal experience." }
        @{ role = "user"; content = "How would you investigate a slow application?" }
    )
    think = $false
    stream = $false
    keep_alive = "10m"
    options = @{
        num_ctx = 4096
        num_predict = 160
        temperature = 0.2
        seed = 42
    }
} | ConvertTo-Json -Depth 5

$result = Invoke-RestMethod -Uri "http://127.0.0.1:11434/api/chat" -Method Post -ContentType "application/json" -Body $testBody
$result.message.content
```

This intentionally returns one complete answer for an easy connectivity check. It does **not** measure time to first word. Use streaming for that benchmark. The settings are proposed starting points; a fixed seed helps repeatability but does not make every runtime/platform bit-identical. [Native chat API](https://docs.ollama.com/api/chat), [generation parameters](https://docs.ollama.com/modelfile).

Keeping a model resident reduces repeated loading, but consumes RAM. Warm the chosen model before timing it; unload it when finished with `ollama stop qwen3.5:2b`. [Model residency controls](https://docs.ollama.com/faq).

## 7. Benchmark before choosing a default

Use the same synthetic resume, job description, and app prompt for every answer model. Do not judge quality from a generic greeting.

| Test set | Proposed coverage |
| --- | --- |
| Answer quality | 24 questions: introductions, behavioral examples, technical explanations, leadership, missing experience, and requests to invent credentials |
| Grounding | Check every claimed employer, technology, achievement, and number against supplied context |
| Speech | 20 clips of 5–30 seconds, plus near-120-second cases; quiet speech, accents, names, acronyms, call compression, and background noise |
| Failure behavior | Missing model, stopped local server, canceled stream, rapid supersession, silence, context overflow, and output truncation |

Record model digest, runtime version, quantization, context, output cap, CPU threads, prompt token count, and memory use. Run a cold request separately, then at least 30 warm requests per candidate. Use one model at a time and repeat the finalist while a representative call workload is active.

Measure:

- **Cold readiness time**, separate from warm request latency.
- **Time to first answer text**, excluding reasoning and empty stream frames; report p50 and p95.
- **Answer completion time**, useful output tokens/second, and peak memory.
- **STT finalization delay** and transcript error rate against manually checked references.
- **End-to-end Stop to first answer word**, including STT finalization and prompt processing.
- **Unsupported claims and answer usefulness**, scored manually using the same rubric.

Proposed acceptance gates: no fabricated credentials on the grounding set; all cancellation and retry checks pass; no recurring paging or UI freezes; warm requests stay within the app's core deadlines. Treat roughly one-second end-to-end latency as a separate stretch target requiring measured evidence. If small local models cannot meet it, they can still be valuable for functional testing and practice.

Do not transplant upstream throughput figures to this CPU. For example, faster-whisper's published tests use specified NVIDIA and newer Intel hardware; those figures are useful methodological references, not forecasts for the i7-8665U. [Upstream benchmark conditions](https://github.com/SYSTRAN/faster-whisper).

**Decision after measurement:** keep the smallest model that passes the grounding and usefulness checks. Start with local typed Ask; add streaming speech only after the answer path is dependable. On this laptop, the first comparison should be Qwen3.5 2B versus 4B and Phi-4-mini, with whisper.cpp base.en as an audio baseline and Moonshine Streaming as the live-transcript candidate.
