#ifndef DSTERM_SHIM_H
#define DSTERM_SHIM_H

#include <stdbool.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/*
 * dsterm shim: a stable, dsterm-owned C API over llama.cpp.
 *
 * Rust never touches llama.cpp structs directly. All handles below are
 * opaque. Every *_load / *_new function returns NULL on failure and never
 * partially constructs a handle.
 *
 * Structs are always built starting from llama_*_default_params() inside the
 * shim -- never from zero-initialized literals -- so fields this shim does
 * not know about keep upstream's safe defaults across llama.cpp bumps.
 */

typedef struct dsterm_ctx_config {
    uint32_t n_ctx;
    uint32_t n_batch;
    uint32_t n_ubatch;
    int32_t n_threads;
    int32_t n_threads_batch;
    int32_t pooling_type;     /* llama_pooling_type */
    bool embeddings;          /* extract embeddings (together with logits) */
    bool flash_attn;          /* maps onto llama flash_attn_type ENABLED/DISABLED */
    bool offload_kqv;
    int32_t rope_scaling_type; /* llama_rope_scaling_type */
} dsterm_ctx_config;

typedef struct dsterm_sampler_config {
    float temperature;         /* 0.0 = skip temperature sampler */
    int32_t top_k;             /* <= 0 = skip */
    float top_p;               /* <= 0.0 or >= 1.0 = skip */
    float min_p;               /* <= 0.0 = skip */
    float repeat_penalty;      /* 1.0 = neutral */
    float frequency_penalty;   /* 0.0 = neutral */
    float presence_penalty;    /* 0.0 = neutral */
    int32_t penalty_last_n;    /* 0 = disable penalties, -1 = full context */
} dsterm_sampler_config;

/* Model */
void *      dsterm_llama_model_load(const char *path);          /* NULL on failure */
const void *dsterm_llama_model_vocab(const void *model);        /* cached vocab handle */
const void *dsterm_llama_model_raw(const void *model);          /* underlying llama_model* */
void        dsterm_llama_model_free(void *model);
int32_t     dsterm_llama_n_embd(const void *model);

/* Context */
void *      dsterm_llama_ctx_new(const void *model, const dsterm_ctx_config *cfg); /* NULL on failure */
void        dsterm_llama_ctx_free(void *ctx);
uint32_t    dsterm_llama_n_ctx(const void *ctx);

/* Vocabulary / tokenization */
int32_t dsterm_llama_tokenize(const void *vocab, const char *text, int32_t len,
                              int32_t *tokens, int32_t max, bool add_bos, bool special);
int32_t dsterm_llama_token_to_piece(const void *vocab, int32_t token,
                                    char *buf, int32_t len, int32_t lstrip, bool special);
int32_t dsterm_llama_token_bos(const void *vocab);
int32_t dsterm_llama_token_eos(const void *vocab);
int32_t dsterm_llama_n_vocab(const void *vocab);

/* Logits / embeddings (last token of the most recent decode) */
float * dsterm_llama_get_logits(void *ctx);
float * dsterm_llama_get_embeddings(void *ctx);

/* Sampling */
void *  dsterm_llama_sampler_new(const dsterm_sampler_config *cfg); /* NULL on failure */
int32_t dsterm_llama_sample(void *sampler, void *ctx);              /* -1 on invalid args */
void    dsterm_llama_sampler_free(void *sampler);

/* Chat templates: llama.cpp's native Jinja2 engine (common/chat.cpp).
 * init reads the GGUF's own embedded template; "" override, never a custom one. */
void *dsterm_chat_templates_init(const void *model);     /* NULL on failure */
void  dsterm_chat_templates_free(void *tmpls);
bool  dsterm_chat_supports_thinking(const void *tmpls);  /* false on invalid handle */

typedef struct dsterm_chat_message {
    const char *role;      /* NUL-terminated */
    const char *content;   /* NUL-terminated */
} dsterm_chat_message;

/* Heap-allocated by the shim; strings owned by this struct. */
typedef struct dsterm_chat_result {
    char  *prompt;                 /* rendered prompt (NUL-terminated) */
    bool   supports_thinking;
    char  *thinking_start_tag;     /* NULL if the template has none */
    char **thinking_end_tags;      /* NULL if none */
    int32_t n_thinking_end_tags;
    char **additional_stops;       /* template-implied stop strings, NULL if none */
    int32_t n_additional_stops;
} dsterm_chat_result;

/* add_bos/add_eos are always left false: BOS is added exactly once at
 * tokenize time on the Rust side (dsterm_llama_tokenize's add_bos flag). */
dsterm_chat_result *dsterm_chat_apply_template(
    const void *tmpls,
    const dsterm_chat_message *messages, int32_t n_messages,
    bool enable_thinking);         /* NULL on failure */
void dsterm_chat_result_free(dsterm_chat_result *result);

#ifdef __cplusplus
}
#endif

#endif /* DSTERM_SHIM_H */
