/*
 * dsterm shim: stable C API over llama.cpp for dsterm's Rust FFI.
 *
 * Every *_load / *_new function returns NULL on failure and never partially
 * constructs a handle. All param structs are built starting from the
 * llama_*_default_params() functions, never from zero-initialized literals.
 */

#include "dsterm_shim.h"

#include <stdlib.h>

#include "llama.h"

typedef struct dsterm_model {
    struct llama_model *model;
    const struct llama_vocab *vocab;
} dsterm_model;

void *dsterm_llama_model_load(const char *path) {
    if (path == NULL) {
        return NULL;
    }

    struct llama_model_params params = llama_model_default_params();
    struct llama_model *model = llama_model_load_from_file(path, params);
    if (model == NULL) {
        return NULL;
    }

    dsterm_model *dm = (dsterm_model *)malloc(sizeof(dsterm_model));
    if (dm == NULL) {
        llama_model_free(model);
        return NULL;
    }
    dm->model = model;
    dm->vocab = llama_model_get_vocab(model);

    return dm;
}

const void *dsterm_llama_model_vocab(const void *model) {
    if (model == NULL) {
        return NULL;
    }
    return ((const dsterm_model *)model)->vocab;
}

void dsterm_llama_model_free(void *model) {
    if (model == NULL) {
        return;
    }
    dsterm_model *dm = (dsterm_model *)model;
    llama_model_free(dm->model);
    free(dm);
}

int32_t dsterm_llama_n_embd(const void *model) {
    if (model == NULL) {
        return 0;
    }
    return llama_model_n_embd(((const dsterm_model *)model)->model);
}

void *dsterm_llama_ctx_new(const void *model, const dsterm_ctx_config *cfg) {
    if (model == NULL || cfg == NULL) {
        return NULL;
    }

    struct llama_context_params params = llama_context_default_params();
    params.n_ctx = cfg->n_ctx;
    params.n_batch = cfg->n_batch;
    params.n_ubatch = cfg->n_ubatch;
    params.n_threads = cfg->n_threads;
    params.n_threads_batch = cfg->n_threads_batch;
    params.pooling_type = (enum llama_pooling_type)cfg->pooling_type;
    params.embeddings = cfg->embeddings;
    params.flash_attn_type = cfg->flash_attn ? LLAMA_FLASH_ATTN_TYPE_ENABLED
                                            : LLAMA_FLASH_ATTN_TYPE_DISABLED;
    params.offload_kqv = cfg->offload_kqv;
    params.rope_scaling_type = (enum llama_rope_scaling_type)cfg->rope_scaling_type;

    return llama_init_from_model(((const dsterm_model *)model)->model, params);
}

void dsterm_llama_ctx_free(void *ctx) {
    if (ctx == NULL) {
        return;
    }
    llama_free((struct llama_context *)ctx);
}

uint32_t dsterm_llama_n_ctx(const void *ctx) {
    if (ctx == NULL) {
        return 0;
    }
    return llama_n_ctx((const struct llama_context *)ctx);
}

int32_t dsterm_llama_tokenize(const void *vocab, const char *text, int32_t len,
                              int32_t *tokens, int32_t max, bool add_bos, bool special) {
    if (vocab == NULL || text == NULL || tokens == NULL) {
        return -1;
    }
    return llama_tokenize((const struct llama_vocab *)vocab, text, len, tokens, max, add_bos, special);
}

int32_t dsterm_llama_token_to_piece(const void *vocab, int32_t token,
                                    char *buf, int32_t len, int32_t lstrip, bool special) {
    if (vocab == NULL || buf == NULL) {
        return -1;
    }
    return llama_token_to_piece((const struct llama_vocab *)vocab, token, buf, len, lstrip, special);
}

int32_t dsterm_llama_token_bos(const void *vocab) {
    if (vocab == NULL) {
        return -1;
    }
    return llama_vocab_bos((const struct llama_vocab *)vocab);
}

int32_t dsterm_llama_token_eos(const void *vocab) {
    if (vocab == NULL) {
        return -1;
    }
    return llama_vocab_eos((const struct llama_vocab *)vocab);
}

int32_t dsterm_llama_n_vocab(const void *vocab) {
    if (vocab == NULL) {
        return 0;
    }
    return llama_vocab_n_tokens((const struct llama_vocab *)vocab);
}

float *dsterm_llama_get_logits(void *ctx) {
    if (ctx == NULL) {
        return NULL;
    }
    return llama_get_logits_ith((struct llama_context *)ctx, -1);
}

float *dsterm_llama_get_embeddings(void *ctx) {
    if (ctx == NULL) {
        return NULL;
    }
    return llama_get_embeddings_ith((struct llama_context *)ctx, -1);
}

void *dsterm_llama_sampler_new(const dsterm_sampler_config *cfg) {
    if (cfg == NULL) {
        return NULL;
    }

    struct llama_sampler *chain = llama_sampler_chain_init(llama_sampler_chain_default_params());
    if (chain == NULL) {
        return NULL;
    }

    /* D3: penalties are wired to actually apply now (penalty_last_n = -1 uses
     * the full context window). The old code passed an empty token history,
     * silently disabling them. */
    if (cfg->penalty_last_n != 0 &&
        (cfg->repeat_penalty != 1.0f || cfg->frequency_penalty != 0.0f ||
         cfg->presence_penalty != 0.0f)) {
        struct llama_sampler *pen = llama_sampler_init_penalties(
            cfg->penalty_last_n, cfg->repeat_penalty, cfg->frequency_penalty,
            cfg->presence_penalty);
        if (pen == NULL) {
            llama_sampler_free(chain);
            return NULL;
        }
        llama_sampler_chain_add(chain, pen);
    }

    if (cfg->temperature != 0.0f) {
        struct llama_sampler *s = llama_sampler_init_temp(cfg->temperature);
        if (s == NULL) {
            llama_sampler_free(chain);
            return NULL;
        }
        llama_sampler_chain_add(chain, s);
    }

    if (cfg->top_k > 0) {
        struct llama_sampler *s = llama_sampler_init_top_k(cfg->top_k);
        if (s == NULL) {
            llama_sampler_free(chain);
            return NULL;
        }
        llama_sampler_chain_add(chain, s);
    }

    if (cfg->top_p > 0.0f && cfg->top_p < 1.0f) {
        struct llama_sampler *s = llama_sampler_init_top_p(cfg->top_p, 1);
        if (s == NULL) {
            llama_sampler_free(chain);
            return NULL;
        }
        llama_sampler_chain_add(chain, s);
    }

    if (cfg->min_p > 0.0f) {
        struct llama_sampler *s = llama_sampler_init_min_p(cfg->min_p, 1);
        if (s == NULL) {
            llama_sampler_free(chain);
            return NULL;
        }
        llama_sampler_chain_add(chain, s);
    }

    struct llama_sampler *last = llama_sampler_init_greedy();
    if (last == NULL) {
        llama_sampler_free(chain);
        return NULL;
    }
    llama_sampler_chain_add(chain, last);

    return chain;
}

int32_t dsterm_llama_sample(void *sampler, void *ctx) {
    if (sampler == NULL || ctx == NULL) {
        return -1;
    }

    /* NOTE: llama_sampler_sample() already calls llama_sampler_accept()
     * internally (see src/llama-sampler.cpp, end of llama_sampler_sample()).
     * Do NOT add an explicit llama_sampler_accept() here -- it would feed
     * every token into the penalty tracker's history twice. */
    return llama_sampler_sample((struct llama_sampler *)sampler, (struct llama_context *)ctx, -1);
}

void dsterm_llama_sampler_free(void *sampler) {
    if (sampler == NULL) {
        return;
    }
    llama_sampler_free((struct llama_sampler *)sampler);
}
