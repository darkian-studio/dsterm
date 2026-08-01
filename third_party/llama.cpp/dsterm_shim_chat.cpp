/*
 * dsterm chat-template shim: exposes llama.cpp's native chat template engine
 * (common/chat.cpp, real Jinja2, the engine behind pocketpal's
 * getFormattedChat) to Rust, using the same opaque-handle pattern as
 * dsterm_shim.c.
 *
 * All functions catch C++ exceptions at the boundary: llama.cpp throws on
 * template parse/render failures, and a thrown exception must never cross
 * into Rust.
 */

#include "dsterm_shim.h"

#include "common/chat.h"

#include <cstdlib>
#include <cstring>
#include <new>
#include <string>
#include <utility>
#include <vector>

extern "C" {

static char *dup_str(const std::string & s) {
    char *p = (char *)malloc(s.size() + 1);
    if (p == NULL) {
        return NULL;
    }
    memcpy(p, s.c_str(), s.size() + 1);
    return p;
}

static char **dup_str_vec(const std::vector<std::string> & v) {
    if (v.empty()) {
        return NULL;
    }
    char **arr = (char **)calloc(v.size(), sizeof(char *));
    if (arr == NULL) {
        return NULL;
    }
    for (size_t i = 0; i < v.size(); i++) {
        arr[i] = dup_str(v[i]);
        if (arr[i] == NULL) {
            for (size_t j = 0; j < i; j++) {
                free(arr[j]);
            }
            free(arr);
            return NULL;
        }
    }
    return arr;
}

static void free_str_vec(char **arr, int32_t n) {
    if (arr == NULL) {
        return;
    }
    for (int32_t i = 0; i < n; i++) {
        free(arr[i]);
    }
    free(arr);
}

void *dsterm_chat_templates_init(const void *model) {
    if (model == NULL) {
        return NULL;
    }
    const struct llama_model *raw =
        (const struct llama_model *)dsterm_llama_model_raw(model);
    if (raw == NULL) {
        return NULL;
    }
    try {
        /* "" override: always the GGUF's own embedded template. */
        return common_chat_templates_init(raw, "").release();
    } catch (...) {
        return NULL;
    }
}

void dsterm_chat_templates_free(void *tmpls) {
    if (tmpls == NULL) {
        return;
    }
    common_chat_templates_free((struct common_chat_templates *)tmpls);
}

bool dsterm_chat_supports_thinking(const void *tmpls) {
    if (tmpls == NULL) {
        return false;
    }
    try {
        return common_chat_templates_support_enable_thinking(
            (const struct common_chat_templates *)tmpls);
    } catch (...) {
        return false;
    }
}

dsterm_chat_result *dsterm_chat_apply_template(
    const void *tmpls,
    const dsterm_chat_message *messages, int32_t n_messages,
    bool enable_thinking) {
    if (tmpls == NULL || (n_messages > 0 && messages == NULL)) {
        return NULL;
    }

    dsterm_chat_result *result =
        (dsterm_chat_result *)calloc(1, sizeof(dsterm_chat_result));
    if (result == NULL) {
        return NULL;
    }

    try {
        std::vector<common_chat_msg> msgs;
        msgs.reserve(n_messages > 0 ? (size_t)n_messages : 0);
        for (int32_t i = 0; i < n_messages; i++) {
            common_chat_msg m;
            m.role    = messages[i].role    != NULL ? messages[i].role    : "";
            m.content = messages[i].content != NULL ? messages[i].content : "";
            msgs.push_back(std::move(m));
        }

        struct common_chat_templates_inputs inputs;
        inputs.messages = std::move(msgs);
        inputs.enable_thinking = enable_thinking;
        /* add_bos/add_eos: keep false. BOS is added exactly once at
         * tokenize time on the Rust side; baking it in here too would
         * double it. "now" keeps the struct's own default (real time). */
        inputs.add_bos = false;
        inputs.add_eos = false;

        struct common_chat_params params = common_chat_templates_apply(
            (const struct common_chat_templates *)tmpls, inputs);

        result->prompt = dup_str(params.prompt);
        if (result->prompt == NULL) {
            throw std::bad_alloc();
        }
        result->supports_thinking = params.supports_thinking;
        if (!params.thinking_start_tag.empty()) {
            result->thinking_start_tag = dup_str(params.thinking_start_tag);
            if (result->thinking_start_tag == NULL) {
                throw std::bad_alloc();
            }
        }
        result->n_thinking_end_tags = (int32_t)params.thinking_end_tags.size();
        result->thinking_end_tags = dup_str_vec(params.thinking_end_tags);
        if (result->n_thinking_end_tags > 0 && result->thinking_end_tags == NULL) {
            throw std::bad_alloc();
        }
        result->n_additional_stops = (int32_t)params.additional_stops.size();
        result->additional_stops = dup_str_vec(params.additional_stops);
        if (result->n_additional_stops > 0 && result->additional_stops == NULL) {
            throw std::bad_alloc();
        }
    } catch (...) {
        dsterm_chat_result_free(result);
        return NULL;
    }

    return result;
}

void dsterm_chat_result_free(dsterm_chat_result *result) {
    if (result == NULL) {
        return;
    }
    free(result->prompt);
    free(result->thinking_start_tag);
    free_str_vec(result->thinking_end_tags, result->n_thinking_end_tags);
    free_str_vec(result->additional_stops, result->n_additional_stops);
    free(result);
}

} /* extern "C" */
