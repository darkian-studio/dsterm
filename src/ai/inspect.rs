use serde_json::{json, Value};

use super::gguf::{
    self, detect_capabilities, estimate_memory, format_file_size, format_parameter_count,
    parse_gguf, GGUFMetadata,
};

pub fn inspect_model(path: &str) -> Result<Value, String> {
    let meta = parse_gguf(path).map_err(|e| format!("GGUF parse failed: {e}"))?;
    Ok(metadata_to_json(&meta))
}

pub fn metadata_to_json(meta: &GGUFMetadata) -> Value {
    let capabilities = detect_capabilities(meta);
    let memory = estimate_memory(meta);
    let arch = meta.architecture.as_deref().unwrap_or("unknown");
    let param_str = meta
        .computed_parameter_count
        .or(meta.parameter_count)
        .map(format_parameter_count)
        .unwrap_or_else(|| "unknown".to_string());

    let mut meta_kv = serde_json::Map::new();
    for (k, v) in &meta.raw_metadata {
        meta_kv.insert(k.clone(), gguf_value_to_json(v));
    }

    let head_dim = meta.head_dim.unwrap_or(0);
    let _ = head_dim;

    json!({
        "model": {
            "id": meta.model_id(),
            "name": meta.model_name.as_deref().unwrap_or("unknown"),
            "path": &meta.file_path,
            "file_size": meta.file_size,
            "file_size_formatted": format_file_size(meta.file_size),
            "parameter_count": param_str,
            "parameter_count_raw": meta.computed_parameter_count.or(meta.parameter_count),
            "quantisation": meta.quantisation.as_deref().unwrap_or("unknown"),
            "format_version": meta.version,
            "tensor_count": meta.tensor_count
        },
        "architecture": {
            "architecture": arch,
            "context_length": meta.context_length.unwrap_or(0),
            "embedding_length": meta.embedding_length.unwrap_or(0),
            "block_count": meta.block_count.unwrap_or(0),
            "head_count": meta.head_count.unwrap_or(0),
            "head_dimension": meta.head_dim.unwrap_or(0),
            "feed_forward_length": meta.feed_forward_length.unwrap_or(0),
            "expert_count": meta.expert_count,
            "expert_used_count": meta.expert_used_count
        },
        "memory": {
            "model_weights_bytes": memory.model_weights_bytes,
            "model_weights_formatted": format_file_size(memory.model_weights_bytes),
            "kv_cache_bytes": memory.kv_cache_bytes,
            "kv_cache_formatted": format_file_size(memory.kv_cache_bytes),
            "overhead_bytes": memory.overhead_bytes,
            "total_bytes": memory.total_bytes,
            "total_formatted": format_file_size(memory.total_bytes),
            "recommended_min_ram_bytes": memory.recommended_min_ram_bytes,
            "recommended_min_ram_formatted": format_file_size(memory.recommended_min_ram_bytes),
            "recommended_ram_bytes": memory.recommended_ram_bytes,
            "recommended_ram_formatted": format_file_size(memory.recommended_ram_bytes),
            "kv_cache_params": {
                "context_length": memory.kv_context_length,
                "layer_count": memory.kv_layer_count,
                "head_count": memory.kv_head_count,
                "head_dimension": memory.kv_head_dim
            }
        },
        "capabilities": {
            "chat": capabilities.chat,
            "completion": capabilities.completion,
            "fim": capabilities.fim,
            "embeddings": capabilities.embeddings,
            "tool_calling": capabilities.tool_calling
        },
        "tokenizer": {
            "tokenizer_model": meta.tokenizer_model.as_deref().unwrap_or("unknown"),
            "chat_template": meta.chat_template.as_deref().unwrap_or(""),
            "bos_token": meta.bos_token_id,
            "eos_token": meta.eos_token_id,
            "pad_token": meta.pad_token_id
        },
        "metadata": meta_kv
    })
}

fn gguf_value_to_json(val: &gguf::GGUFValue) -> Value {
    match val {
        gguf::GGUFValue::Uint8(v) => json!(v),
        gguf::GGUFValue::Int8(v) => json!(v),
        gguf::GGUFValue::Uint16(v) => json!(v),
        gguf::GGUFValue::Int16(v) => json!(v),
        gguf::GGUFValue::Uint32(v) => json!(v),
        gguf::GGUFValue::Int32(v) => json!(v),
        gguf::GGUFValue::Float32(v) => json!(v),
        gguf::GGUFValue::Bool(v) => json!(v),
        gguf::GGUFValue::String(v) => json!(v),
        gguf::GGUFValue::Array(arr) => {
            json!(arr.iter().map(gguf_value_to_json).collect::<Vec<_>>())
        }
        gguf::GGUFValue::Uint64(v) => json!(v),
        gguf::GGUFValue::Int64(v) => json!(v),
        gguf::GGUFValue::Float64(v) => json!(v),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_gguf_value_to_json() {
        let v = gguf_value_to_json(&gguf::GGUFValue::String("hello".into()));
        assert_eq!(v, json!("hello"));
        let v = gguf_value_to_json(&gguf::GGUFValue::Uint32(42));
        assert_eq!(v, json!(42));
        let v = gguf_value_to_json(&gguf::GGUFValue::Bool(true));
        assert_eq!(v, json!(true));
    }

    #[test]
    fn test_metadata_to_json_has_structured_sections() {
        let mut raw = HashMap::new();
        raw.insert(
            "general.source.huggingface.repository".into(),
            gguf::GGUFValue::String("bartowski/Qwen3-4B-Instruct-GGUF".into()),
        );

        let meta = GGUFMetadata {
            version: 3,
            tensor_count: 0,
            architecture: Some("qwen2".into()),
            model_name: Some("Qwen3-4B-Instruct".into()),
            file_type: Some(15),
            context_length: Some(32768),
            embedding_length: Some(2560),
            block_count: Some(28),
            head_count: Some(20),
            head_dim: Some(128),
            feed_forward_length: Some(8960),
            expert_count: None,
            expert_used_count: None,
            parameter_count: Some(3_900_000_000.0),
            computed_parameter_count: None,
            file_size: 2_500_000_000,
            file_path: "/models/test.gguf".into(),
            quantisation: Some("Q4_K_M".into()),
            raw_metadata: raw,
            tokenizer_model: Some("gpt2".into()),
            chat_template: Some("{{ messages }}".into()),
            bos_token_id: Some(151643),
            eos_token_id: Some(151643),
            pad_token_id: Some(0),
        };
        let json = metadata_to_json(&meta);

        assert_eq!(json["model"]["id"], "hf://bartowski/Qwen3-4B-Instruct-GGUF");
        assert_eq!(json["model"]["name"], "Qwen3-4B-Instruct");
        assert_eq!(json["model"]["quantisation"], "Q4_K_M");
        assert_eq!(json["model"]["parameter_count"], "3.90B");
        assert_eq!(json["model"]["format_version"], 3);

        assert_eq!(json["architecture"]["architecture"], "qwen2");
        assert_eq!(json["architecture"]["context_length"], 32768);
        assert_eq!(json["architecture"]["head_dimension"], 128);

        assert!(json["memory"]["total_bytes"].as_u64().unwrap() > 0);
        assert!(
            json["memory"]["recommended_min_ram_bytes"]
                .as_u64()
                .unwrap()
                > 0
        );
        assert!(json["memory"]["recommended_ram_bytes"].as_u64().unwrap() > 0);

        assert!(json["capabilities"]["chat"].as_bool().unwrap_or(false));
        assert!(json["capabilities"]["tool_calling"]
            .as_bool()
            .unwrap_or(false));

        assert_eq!(json["tokenizer"]["tokenizer_model"], "gpt2");
        assert_eq!(json["tokenizer"]["bos_token"], 151643);

        assert!(json["metadata"]["general.source.huggingface.repository"].is_string());
    }

    #[test]
    fn test_metadata_to_json_local_model_id() {
        let meta = GGUFMetadata {
            version: 3,
            tensor_count: 0,
            architecture: Some("llama".into()),
            model_name: Some("My-Model".into()),
            file_type: Some(8),
            context_length: None,
            embedding_length: None,
            block_count: None,
            head_count: None,
            head_dim: None,
            feed_forward_length: None,
            expert_count: None,
            expert_used_count: None,
            parameter_count: None,
            computed_parameter_count: None,
            file_size: 0,
            file_path: "/local/path.gguf".into(),
            quantisation: Some("Q8_0".into()),
            raw_metadata: HashMap::new(),
            tokenizer_model: None,
            chat_template: None,
            bos_token_id: None,
            eos_token_id: None,
            pad_token_id: None,
        };
        let json = metadata_to_json(&meta);
        let id = json["model"]["id"].as_str().unwrap().to_string();
        assert!(
            id.starts_with("local://"),
            "expected local:// prefix, got {id}"
        );
        assert!(id.contains("llama"));
        assert!(id.contains("My-Model"));
    }
}
