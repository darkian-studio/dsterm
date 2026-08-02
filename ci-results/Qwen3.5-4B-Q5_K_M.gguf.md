# Model capacity verification

- **model**: `/home/runner/work/_temp/models/Qwen3.5-4B-Q5_K_M.gguf`
- **date (UTC)**: 2026-08-02 18:06:35
- **runner**: Linux-6.17.0-1020-azure-x86_64-with-glibc2.39 / x86_64, 4 cpus, 15.6 GiB RAM
- **sampler**: temperature 0.55 (app slider); shim chain penalties -> temp -> top_k -> top_p -> min_p -> **greedy**, no dist() -> deterministic
- **max completion tokens per test**: 256

| test | prompt tokens | completion tokens | TTFT (s) | wall (s) | verdict | reply |
|---|---|---|---|---|---|---|
| A | 28 | 256 | 2.6 | 5.2 | ok | Hey there! How can I help you today? 😊 |
| S | 48 | 256 | 4.3 | 6.2 | ok | Hello! How can I help you today? Feel free to ask a question |
| H | 673 | 14 | 58.1 | 59.7 | ok | What can I help you with? ⏎ <\|im_end\| |
| N | 1299 | 14 | 112.4 | 113.9 | ok | What can I help you with? ⏎ <\|im_end\| |
| P | 1295 | 17 | 112.0 | 114.0 | ok | How can I assist you with your code today? ⏎ <\|im_end\| |
| N2 | 1299 | 14 | 112.7 | 114.2 | ok | What can I help you with? ⏎ <\|im_end\| |

**Determinism (N vs N2 byte-identical): PASS**
**N vs P style pair: N=1299 tok, P=1295 tok**
**peak RAM at first token (test N): 1.87 / 15.61 GiB**
