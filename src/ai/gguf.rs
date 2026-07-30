use serde::Serialize;
use std::collections::HashMap;
use std::io::{Cursor, Read};

#[derive(Debug, Clone)]
pub enum GGUFValue {
    Uint8(u8),
    Int8(i8),
    Uint16(u16),
    Int16(i16),
    Uint32(u32),
    Int32(i32),
    Float32(f32),
    Bool(bool),
    String(String),
    Array(Vec<GGUFValue>),
    Uint64(u64),
    Int64(i64),
    Float64(f64),
}

impl GGUFValue {
    pub fn as_u32(&self) -> Option<u32> {
        match self {
            GGUFValue::Uint32(v) => Some(*v),
            GGUFValue::Uint8(v) => Some(*v as u32),
            GGUFValue::Uint16(v) => Some(*v as u32),
            GGUFValue::Uint64(v) => Some(*v as u32),
            GGUFValue::Int32(v) => Some(*v as u32),
            GGUFValue::Int64(v) => Some(*v as u32),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            GGUFValue::Int64(v) => Some(*v),
            GGUFValue::Int32(v) => Some(*v as i64),
            GGUFValue::Uint32(v) => Some(*v as i64),
            GGUFValue::Uint64(v) => Some(*v as i64),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        if let GGUFValue::String(s) = self {
            Some(s.as_str())
        } else {
            None
        }
    }
}

#[derive(Debug, Clone)]
pub struct GGUFMetadata {
    pub version: u32,
    pub tensor_count: u64,
    pub architecture: Option<String>,
    pub model_name: Option<String>,
    pub file_type: Option<u32>,
    pub context_length: Option<u32>,
    pub embedding_length: Option<u32>,
    pub block_count: Option<u32>,
    pub head_count: Option<u32>,
    pub head_dim: Option<u32>,
    pub feed_forward_length: Option<u32>,
    pub expert_count: Option<u32>,
    pub expert_used_count: Option<u32>,
    pub parameter_count: Option<f64>,
    pub computed_parameter_count: Option<f64>,
    pub file_size: u64,
    pub file_path: String,
    pub quantisation: Option<String>,
    pub raw_metadata: HashMap<String, GGUFValue>,
    pub tokenizer_model: Option<String>,
    pub chat_template: Option<String>,
    pub bos_token_id: Option<i64>,
    pub eos_token_id: Option<i64>,
    pub pad_token_id: Option<i64>,
}

impl GGUFMetadata {
    pub fn model_id(&self) -> String {
        let hf_repo = self
            .raw_metadata
            .get("general.source.huggingface.repository")
            .and_then(|v| v.as_str());
        if let Some(repo) = hf_repo {
            return format!("hf://{repo}");
        }
        let arch = self.architecture.as_deref().unwrap_or("unknown");
        let name = self.model_name.as_deref().unwrap_or("unknown");
        let q = self.quantisation.as_deref().unwrap_or("unknown");
        format!("local://{arch}/{name}/{q}")
    }
}

#[derive(Debug, Clone)]
pub enum GGUFError {
    InvalidMagic,
    UnsupportedVersion(u32),
    Io(String),
    InvalidMetadata(String),
}

impl std::fmt::Display for GGUFError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GGUFError::InvalidMagic => write!(f, "invalid GGUF magic number"),
            GGUFError::UnsupportedVersion(v) => write!(f, "unsupported GGUF version: {v}"),
            GGUFError::Io(e) => write!(f, "I/O error: {e}"),
            GGUFError::InvalidMetadata(e) => write!(f, "invalid metadata: {e}"),
        }
    }
}

impl std::error::Error for GGUFError {}

fn read_u32_le(cursor: &mut Cursor<&[u8]>) -> Result<u32, GGUFError> {
    let mut buf = [0u8; 4];
    cursor
        .read_exact(&mut buf)
        .map_err(|e| GGUFError::Io(e.to_string()))?;
    Ok(u32::from_le_bytes(buf))
}

fn read_u64_le(cursor: &mut Cursor<&[u8]>) -> Result<u64, GGUFError> {
    let mut buf = [0u8; 8];
    cursor
        .read_exact(&mut buf)
        .map_err(|e| GGUFError::Io(e.to_string()))?;
    Ok(u64::from_le_bytes(buf))
}

fn read_gguf_string(cursor: &mut Cursor<&[u8]>) -> Result<String, GGUFError> {
    let len = read_u64_le(cursor)? as usize;
    let mut buf = vec![0u8; len];
    cursor
        .read_exact(&mut buf)
        .map_err(|e| GGUFError::Io(e.to_string()))?;
    String::from_utf8(buf)
        .map_err(|_| GGUFError::InvalidMetadata("invalid UTF-8 in GGUF string".into()))
}

fn read_value(cursor: &mut Cursor<&[u8]>, value_type: u32) -> Result<GGUFValue, GGUFError> {
    match value_type {
        0 => {
            let mut buf = [0u8; 1];
            cursor
                .read_exact(&mut buf)
                .map_err(|e| GGUFError::Io(e.to_string()))?;
            Ok(GGUFValue::Uint8(buf[0]))
        }
        1 => {
            let mut buf = [0u8; 1];
            cursor
                .read_exact(&mut buf)
                .map_err(|e| GGUFError::Io(e.to_string()))?;
            Ok(GGUFValue::Int8(i8::from_le_bytes(buf)))
        }
        2 => {
            let mut buf = [0u8; 2];
            cursor
                .read_exact(&mut buf)
                .map_err(|e| GGUFError::Io(e.to_string()))?;
            Ok(GGUFValue::Uint16(u16::from_le_bytes(buf)))
        }
        3 => {
            let mut buf = [0u8; 2];
            cursor
                .read_exact(&mut buf)
                .map_err(|e| GGUFError::Io(e.to_string()))?;
            Ok(GGUFValue::Int16(i16::from_le_bytes(buf)))
        }
        4 => read_u32_le(cursor).map(GGUFValue::Uint32),
        5 => {
            let mut buf = [0u8; 4];
            cursor
                .read_exact(&mut buf)
                .map_err(|e| GGUFError::Io(e.to_string()))?;
            Ok(GGUFValue::Int32(i32::from_le_bytes(buf)))
        }
        6 => {
            let mut buf = [0u8; 4];
            cursor
                .read_exact(&mut buf)
                .map_err(|e| GGUFError::Io(e.to_string()))?;
            Ok(GGUFValue::Float32(f32::from_le_bytes(buf)))
        }
        7 => {
            let mut buf = [0u8; 1];
            cursor
                .read_exact(&mut buf)
                .map_err(|e| GGUFError::Io(e.to_string()))?;
            Ok(GGUFValue::Bool(buf[0] != 0))
        }
        8 => read_gguf_string(cursor).map(GGUFValue::String),
        9 => {
            let elem_type = read_u32_le(cursor)?;
            let count = read_u64_le(cursor)?;
            let mut elems = Vec::with_capacity(count as usize);
            for _ in 0..count {
                elems.push(read_value(cursor, elem_type)?);
            }
            Ok(GGUFValue::Array(elems))
        }
        10 => read_u64_le(cursor).map(GGUFValue::Uint64),
        11 => {
            let mut buf = [0u8; 8];
            cursor
                .read_exact(&mut buf)
                .map_err(|e| GGUFError::Io(e.to_string()))?;
            Ok(GGUFValue::Int64(i64::from_le_bytes(buf)))
        }
        12 => {
            let mut buf = [0u8; 8];
            cursor
                .read_exact(&mut buf)
                .map_err(|e| GGUFError::Io(e.to_string()))?;
            Ok(GGUFValue::Float64(f64::from_le_bytes(buf)))
        }
        _ => Err(GGUFError::InvalidMetadata(format!(
            "unknown value type: {value_type}"
        ))),
    }
}

pub fn parse_gguf<P: AsRef<std::path::Path>>(path: P) -> Result<GGUFMetadata, GGUFError> {
    let path_str = path.as_ref().display().to_string();
    let data = std::fs::read(path.as_ref()).map_err(|e| GGUFError::Io(e.to_string()))?;
    let file_size = data.len() as u64;
    let mut cursor = Cursor::new(data.as_slice());

    let mut magic = [0u8; 4];
    cursor
        .read_exact(&mut magic)
        .map_err(|e| GGUFError::Io(e.to_string()))?;
    if &magic != b"GGUF" {
        return Err(GGUFError::InvalidMagic);
    }

    let version = read_u32_le(&mut cursor)?;
    if !(1..=3).contains(&version) {
        return Err(GGUFError::UnsupportedVersion(version));
    }

    let tensor_count = read_u64_le(&mut cursor)?;
    let metadata_kv_count = read_u64_le(&mut cursor)?;

    let mut raw_metadata: HashMap<String, GGUFValue> = HashMap::new();
    for _ in 0..metadata_kv_count {
        let key = read_gguf_string(&mut cursor)?;
        let value_type = read_u32_le(&mut cursor)?;
        let value = read_value(&mut cursor, value_type)?;
        raw_metadata.insert(key, value);
    }

    let arch_key = "general.architecture";
    let architecture = raw_metadata
        .get(arch_key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let name_key = format!("{}.name", architecture.as_deref().unwrap_or("general"));
    let model_name = raw_metadata
        .get("general.name")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            raw_metadata
                .get(&name_key)
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        });

    let file_type = raw_metadata
        .get("general.file_type")
        .and_then(|v| v.as_u32());

    let quantisation = file_type.and_then(quantisation_from_ftype);

    let ctx_key = architecture
        .as_ref()
        .map_or("llama.context_length".to_string(), |a| {
            format!("{a}.context_length")
        });
    let context_length = raw_metadata
        .get(&ctx_key)
        .and_then(|v| v.as_u32())
        .or_else(|| {
            raw_metadata
                .get("llama.context_length")
                .and_then(|v| v.as_u32())
        })
        .or_else(|| {
            raw_metadata
                .get("gptneox.context_length")
                .and_then(|v| v.as_u32())
        });

    let embd_key = architecture
        .as_ref()
        .map_or("llama.embedding_length".to_string(), |a| {
            format!("{a}.embedding_length")
        });
    let embedding_length = raw_metadata
        .get(&embd_key)
        .and_then(|v| v.as_u32())
        .or_else(|| {
            raw_metadata
                .get("llama.embedding_length")
                .and_then(|v| v.as_u32())
        });

    let block_key = architecture
        .as_ref()
        .map_or("llama.block_count".to_string(), |a| {
            format!("{a}.block_count")
        });
    let block_count = raw_metadata
        .get(&block_key)
        .and_then(|v| v.as_u32())
        .or_else(|| {
            raw_metadata
                .get("llama.block_count")
                .and_then(|v| v.as_u32())
        });

    let head_key = architecture
        .as_ref()
        .map_or("llama.head_count".to_string(), |a| {
            format!("{a}.head_count")
        });
    let head_count = raw_metadata
        .get(&head_key)
        .and_then(|v| v.as_u32())
        .or_else(|| {
            raw_metadata
                .get("llama.head_count")
                .and_then(|v| v.as_u32())
        });

    let head_dim_key = architecture
        .as_ref()
        .map_or("llama.head_dim".to_string(), |a| format!("{a}.head_dim"));
    let head_dim = raw_metadata.get(&head_dim_key).and_then(|v| v.as_u32());

    let ffn_key = architecture
        .as_ref()
        .map_or("llama.feed_forward_length".to_string(), |a| {
            format!("{a}.feed_forward_length")
        });
    let feed_forward_length = raw_metadata.get(&ffn_key).and_then(|v| v.as_u32());

    let expert_count_key = architecture
        .as_ref()
        .map_or("llama.expert_count".to_string(), |a| {
            format!("{a}.expert_count")
        });
    let expert_count = raw_metadata.get(&expert_count_key).and_then(|v| v.as_u32());

    let expert_used_key = architecture
        .as_ref()
        .map_or("llama.expert_used_count".to_string(), |a| {
            format!("{a}.expert_used_count")
        });
    let expert_used_count = raw_metadata.get(&expert_used_key).and_then(|v| v.as_u32());

    let param_count_key = "general.parameter_count";
    let parameter_count = raw_metadata.get(param_count_key).and_then(|v| match v {
        GGUFValue::Float64(v) => Some(*v),
        GGUFValue::Uint64(v) => Some(*v as f64),
        GGUFValue::Int64(v) => Some(*v as f64),
        GGUFValue::Float32(v) => Some(*v as f64),
        GGUFValue::Uint32(v) => Some(*v as f64),
        GGUFValue::Int32(v) => Some(*v as f64),
        _ => None,
    });

    let computed_parameter_count = compute_parameter_count(&mut cursor, tensor_count, version)?;

    let tokenizer_model = raw_metadata
        .get("tokenizer.ggml.model")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let chat_template = raw_metadata
        .get("tokenizer.ggml.chat_template")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let bos_token_id = raw_metadata
        .get("tokenizer.ggml.bos_token_id")
        .and_then(|v| v.as_i64());
    let eos_token_id = raw_metadata
        .get("tokenizer.ggml.eos_token_id")
        .and_then(|v| v.as_i64());
    let pad_token_id = raw_metadata
        .get("tokenizer.ggml.pad_token_id")
        .and_then(|v| v.as_i64());

    Ok(GGUFMetadata {
        version,
        tensor_count,
        architecture,
        model_name,
        file_type,
        context_length,
        embedding_length,
        block_count,
        head_count,
        head_dim,
        feed_forward_length,
        expert_count,
        expert_used_count,
        parameter_count,
        computed_parameter_count,
        file_size,
        file_path: path_str,
        quantisation,
        raw_metadata,
        tokenizer_model,
        chat_template,
        bos_token_id,
        eos_token_id,
        pad_token_id,
    })
}

fn compute_parameter_count(
    cursor: &mut Cursor<&[u8]>,
    tensor_count: u64,
    _version: u32,
) -> Result<Option<f64>, GGUFError> {
    let mut total_params: f64 = 0.0;
    for _ in 0..tensor_count {
        let _name = read_gguf_string(cursor)?;
        let n_dims = read_u32_le(cursor)?;
        let mut dims = Vec::with_capacity(n_dims as usize);
        for _ in 0..n_dims {
            dims.push(read_u64_le(cursor)?);
        }
        let _tensor_type = read_u32_le(cursor)?;

        let mut tensor_params: f64 = 1.0;
        for d in &dims {
            tensor_params *= *d as f64;
        }
        total_params += tensor_params;
    }
    if total_params > 0.0 {
        Ok(Some(total_params))
    } else {
        Ok(None)
    }
}

fn quantisation_from_ftype(ftype: u32) -> Option<String> {
    let q = match ftype {
        0 => "Q4_0",
        1 => "Q4_1",
        2 => "Q4_1",
        3 => "Q4_1",
        6 => "Q5_0",
        7 => "Q5_1",
        8 => "Q8_0",
        10 => "Q2_K",
        11 => "Q3_K_S",
        12 => "Q3_K_M",
        13 => "Q3_K_L",
        14 => "Q4_K_S",
        15 => "Q4_K_M",
        16 => "Q5_K_S",
        17 => "Q5_K_M",
        18 => "Q6_K",
        19 => "IQ2_XXS",
        20 => "IQ2_XS",
        21 => "IQ3_XXS",
        22 => "IQ1_S",
        23 => "IQ4_NL",
        24 => "IQ3_S",
        25 => "IQ3_M",
        26 => "IQ2_S",
        27 => "IQ2_M",
        28 => "IQ4_XS",
        29 => "IQ1_M",
        30 => "BF16",
        31 => "Q4_0_4_4",
        32 => "Q4_0_4_8",
        33 => "Q4_0_8_8",
        _ => return None,
    };
    Some(q.to_string())
}

pub fn bytes_per_param(quantisation: &str) -> f64 {
    let q = quantisation.to_lowercase();
    if q.contains("iq1") {
        0.125
    } else if q.contains("iq2") {
        0.25
    } else if q.contains("iq3") {
        0.375
    } else if q.contains("iq4") {
        0.5
    } else if q.contains("q2") {
        0.27
    } else if q.contains("q3") {
        0.34
    } else if q.contains("q4") {
        0.5
    } else if q.contains("q5") {
        0.625
    } else if q.contains("q6") {
        0.75
    } else if q.contains("q8") {
        1.0
    } else if q.contains("bf16") || q.contains("f16") {
        2.0
    } else if q.contains("f32") {
        4.0
    } else {
        1.0
    }
}

pub fn estimate_memory(meta: &GGUFMetadata) -> MemoryEstimate {
    let bpp = meta
        .quantisation
        .as_deref()
        .map(bytes_per_param)
        .unwrap_or(1.0);
    let param_count = meta
        .computed_parameter_count
        .or(meta.parameter_count)
        .unwrap_or(0.0);

    let model_weights = if param_count > 0.0 {
        (param_count * bpp) as u64
    } else {
        meta.file_size.saturating_mul(11) / 10
    };

    let ctx = meta.context_length.unwrap_or(4096) as u64;
    let layers = meta.block_count.unwrap_or(32) as u64;
    let heads = meta.head_count.unwrap_or(32) as u64;
    let hd = meta.head_dim.unwrap_or_else(|| {
        meta.embedding_length
            .map(|e| e / heads as u32)
            .unwrap_or(128)
    }) as u64;

    let kv_cache_size = if hd > 0 && heads > 0 && layers > 0 {
        4u64.saturating_mul(layers)
            .saturating_mul(heads)
            .saturating_mul(hd)
            .saturating_mul(ctx)
    } else {
        0
    };

    let overhead = 50_000_000u64;
    let total = model_weights
        .saturating_add(kv_cache_size)
        .saturating_add(overhead);

    let recommended_min = total;
    let recommended = total.saturating_mul(5) / 4;

    MemoryEstimate {
        model_weights_bytes: model_weights,
        kv_cache_bytes: kv_cache_size,
        overhead_bytes: overhead,
        total_bytes: total,
        recommended_min_ram_bytes: recommended_min,
        recommended_ram_bytes: recommended,
        kv_context_length: meta.context_length.unwrap_or(4096),
        kv_layer_count: meta.block_count.unwrap_or(32),
        kv_head_count: meta.head_count.unwrap_or(32),
        kv_head_dim: hd as u32,
    }
}

#[derive(Debug, Clone)]
pub struct MemoryEstimate {
    pub model_weights_bytes: u64,
    pub kv_cache_bytes: u64,
    pub overhead_bytes: u64,
    pub total_bytes: u64,
    pub recommended_min_ram_bytes: u64,
    pub recommended_ram_bytes: u64,
    pub kv_context_length: u32,
    pub kv_layer_count: u32,
    pub kv_head_count: u32,
    pub kv_head_dim: u32,
}

pub fn detect_capabilities(meta: &GGUFMetadata) -> ModelCapabilities {
    let arch = meta.architecture.as_deref().unwrap_or("").to_lowercase();
    let has_chat_template = meta.chat_template.is_some();

    let chat = has_chat_template;
    let completion = true;
    let embeddings = matches!(
        arch.as_str(),
        "bert" | "nomic-bert" | "jina-bert" | "jina-embeddings-v2" | "gte" | "gte-small"
    );
    let fim = matches!(
        arch.as_str(),
        "codellama" | "starcoder" | "starcoder2" | "codegeex4" | "deepseek2"
    );
    let tool_calling = has_chat_template
        && matches!(
            arch.as_str(),
            "llama"
                | "qwen2"
                | "qwen2moe"
                | "mistral"
                | "mixtral"
                | "deepseek2"
                | "gemma2"
                | "phi3"
                | "command-r"
        );
    let reasoning = has_chat_template
        && matches!(arch.as_str(), "deepseek2" | "qwen2");
    let vision = matches!(arch.as_str(), "llava" | "llava-llama" | "qwen2-vl");

    ModelCapabilities {
        chat,
        completion,
        fim,
        embeddings,
        tool_calling,
        reasoning,
        vision,
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelCapabilities {
    pub chat: bool,
    pub completion: bool,
    pub fim: bool,
    pub embeddings: bool,
    pub tool_calling: bool,
    pub reasoning: bool,
    pub vision: bool,
}

impl ModelCapabilities {
    pub fn from_json(val: &serde_json::Value) -> Self {
        Self {
            chat: val["chat"].as_bool().unwrap_or(false),
            completion: val["completion"].as_bool().unwrap_or(true),
            fim: val["fim"].as_bool().unwrap_or(false),
            embeddings: val["embeddings"].as_bool().unwrap_or(false),
            tool_calling: val["tool_calling"].as_bool().unwrap_or(false),
            reasoning: val["reasoning"].as_bool().unwrap_or(false),
            vision: val["vision"].as_bool().unwrap_or(false),
        }
    }
}

pub fn format_file_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

pub fn format_parameter_count(count: f64) -> String {
    if count >= 1_000_000_000_000.0 {
        format!("{:.2}T", count / 1_000_000_000_000.0)
    } else if count >= 1_000_000_000.0 {
        format!("{:.2}B", count / 1_000_000_000.0)
    } else if count >= 1_000_000.0 {
        format!("{:.2}M", count / 1_000_000.0)
    } else {
        format!("{:.0}", count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_quantisation_from_ftype() {
        assert_eq!(quantisation_from_ftype(0).unwrap(), "Q4_0");
        assert_eq!(quantisation_from_ftype(15).unwrap(), "Q4_K_M");
        assert_eq!(quantisation_from_ftype(18).unwrap(), "Q6_K");
        assert_eq!(quantisation_from_ftype(99), None);
    }

    #[test]
    fn test_bytes_per_param() {
        assert!((bytes_per_param("Q4_K_M") - 0.5).abs() < 0.01);
        assert!((bytes_per_param("Q8_0") - 1.0).abs() < 0.01);
        assert!((bytes_per_param("F16") - 2.0).abs() < 0.01);
        assert!((bytes_per_param("Q2_K") - 0.27).abs() < 0.01);
    }

    #[test]
    fn test_format_file_size() {
        assert_eq!(format_file_size(500), "500 B");
        assert_eq!(format_file_size(2048), "2.0 KB");
        assert_eq!(format_file_size(2_500_000), "2.4 MB");
        assert_eq!(format_file_size(2_500_000_000), "2.33 GB");
    }

    #[test]
    fn test_format_parameter_count() {
        assert_eq!(format_parameter_count(500_000_000.0), "500.00M");
        assert_eq!(format_parameter_count(7_000_000_000.0), "7.00B");
        assert_eq!(format_parameter_count(72_000_000_000.0), "72.00B");
    }

    #[test]
    fn test_detect_capabilities_known_arch() {
        let meta = GGUFMetadata {
            version: 3,
            tensor_count: 0,
            architecture: Some("llama".into()),
            model_name: Some("test".into()),
            file_type: Some(15),
            context_length: Some(4096),
            embedding_length: Some(4096),
            block_count: Some(32),
            head_count: Some(32),
            head_dim: Some(128),
            feed_forward_length: None,
            expert_count: None,
            expert_used_count: None,
            parameter_count: None,
            computed_parameter_count: None,
            file_size: 0,
            file_path: "".into(),
            quantisation: Some("Q4_K_M".into()),
            raw_metadata: HashMap::new(),
            tokenizer_model: Some("gpt2".into()),
            chat_template: Some("{{ messages }}".into()),
            bos_token_id: Some(1),
            eos_token_id: Some(2),
            pad_token_id: Some(0),
        };
        let caps = detect_capabilities(&meta);
        assert!(caps.chat);
        assert!(caps.completion);
        assert!(!caps.embeddings);
        assert!(caps.tool_calling);
    }

    #[test]
    fn test_detect_capabilities_embedding_model() {
        let meta = GGUFMetadata {
            version: 3,
            tensor_count: 0,
            architecture: Some("bert".into()),
            model_name: None,
            file_type: None,
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
            file_path: "".into(),
            quantisation: None,
            raw_metadata: HashMap::new(),
            tokenizer_model: None,
            chat_template: None,
            bos_token_id: None,
            eos_token_id: None,
            pad_token_id: None,
        };
        let caps = detect_capabilities(&meta);
        assert!(!caps.chat);
        assert!(caps.completion);
        assert!(caps.embeddings);
        assert!(!caps.tool_calling);
    }

    #[test]
    fn test_parse_nonexistent_file() {
        let result = parse_gguf("/nonexistent/file.gguf");
        assert!(result.is_err());
    }

    #[test]
    fn test_estimate_memory_no_data() {
        let meta = default_meta();
        let est = estimate_memory(&meta);
        assert_eq!(est.model_weights_bytes, 2_200_000_000);
        assert!(est.overhead_bytes > 0);
        assert!(est.recommended_min_ram_bytes > 0);
        assert!(est.recommended_ram_bytes > est.recommended_min_ram_bytes);
    }

    #[test]
    fn test_estimate_memory_with_known_model() {
        let mut meta = default_meta();
        meta.architecture = Some("qwen2".into());
        meta.quantisation = Some("Q4_K_M".into());
        meta.parameter_count = Some(3_900_000_000.0);
        meta.computed_parameter_count = Some(3_900_000_000.0);
        meta.context_length = Some(32768);
        meta.block_count = Some(28);
        meta.head_count = Some(20);
        meta.head_dim = Some(128);

        let est = estimate_memory(&meta);
        assert!(est.model_weights_bytes > 1_000_000_000);
        assert!(est.kv_cache_bytes > 100_000_000);
        assert!(est.total_bytes > est.model_weights_bytes);
    }

    #[test]
    fn test_model_id_from_repo_metadata() {
        let mut meta = default_meta();
        meta.architecture = Some("qwen2".into());
        meta.model_name = Some("Qwen3-4B-Instruct".into());
        meta.quantisation = Some("Q4_K_M".into());
        meta.raw_metadata.insert(
            "general.source.huggingface.repository".into(),
            GGUFValue::String("bartowski/Qwen3-4B-Instruct-GGUF".into()),
        );
        assert_eq!(meta.model_id(), "hf://bartowski/Qwen3-4B-Instruct-GGUF");
    }

    #[test]
    fn test_model_id_fallback_to_local() {
        let meta = GGUFMetadata {
            version: 3,
            tensor_count: 0,
            architecture: Some("llama".into()),
            model_name: Some("Test-7B".into()),
            file_type: Some(15),
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
            file_path: "".into(),
            quantisation: Some("Q4_K_M".into()),
            raw_metadata: HashMap::new(),
            tokenizer_model: None,
            chat_template: None,
            bos_token_id: None,
            eos_token_id: None,
            pad_token_id: None,
        };
        let id = meta.model_id();
        assert!(id.starts_with("local://"));
        assert!(id.contains("llama"));
        assert!(id.contains("Test-7B"));
        assert!(id.contains("Q4_K_M"));
    }

    #[test]
    fn test_quantisation_every_format_maps() {
        let known = [
            0, 1, 2, 3, 6, 7, 8, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25,
            26, 27, 28, 29, 30, 31, 32, 33,
        ];
        for &ft in &known {
            let q = quantisation_from_ftype(ft);
            assert!(
                q.is_some(),
                "ftype {ft} should map to a quantisation string"
            );
            let qs = q.unwrap();
            assert!(!qs.is_empty(), "ftype {ft} should produce non-empty string");
        }
    }

    #[test]
    fn test_bytes_per_param_known_values() {
        assert!((bytes_per_param("Q4_K_M") - 0.5).abs() < 0.01);
        assert!((bytes_per_param("Q8_0") - 1.0).abs() < 0.01);
        assert!((bytes_per_param("F16") - 2.0).abs() < 0.01);
        assert!((bytes_per_param("BF16") - 2.0).abs() < 0.01);
        assert!((bytes_per_param("Q2_K") - 0.27).abs() < 0.01);
        assert!((bytes_per_param("Q3_K_M") - 0.34).abs() < 0.01);
        assert!((bytes_per_param("Q5_K_M") - 0.625).abs() < 0.01);
        assert!((bytes_per_param("Q6_K") - 0.75).abs() < 0.01);
        assert!((bytes_per_param("F32") - 4.0).abs() < 0.01);
        assert!((bytes_per_param("IQ1_S") - 0.125).abs() < 0.01);
        assert!((bytes_per_param("IQ2_XXS") - 0.25).abs() < 0.01);
        assert!((bytes_per_param("IQ3_XXS") - 0.375).abs() < 0.01);
        assert!((bytes_per_param("IQ4_NL") - 0.5).abs() < 0.01);
    }

    #[test]
    fn test_bytes_per_param_unknown_fallback() {
        assert!((bytes_per_param("SOME_UNKNOWN_TYPE") - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_parse_empty_file() {
        let dir = std::env::temp_dir();
        let path = dir.join("empty.gguf");
        let _ = std::fs::write(&path, &[] as &[u8]);
        let result = parse_gguf(&path);
        assert!(result.is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_parse_truncated_header() {
        let dir = std::env::temp_dir();
        let path = dir.join("truncated_header.gguf");
        let _ = std::fs::write(&path, b"GG");
        let result = parse_gguf(&path);
        assert!(result.is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_parse_invalid_magic() {
        let dir = std::env::temp_dir();
        let path = dir.join("bad_magic.gguf");
        let _ = std::fs::write(&path, b"NOTG");
        let result = parse_gguf(&path);
        assert!(matches!(result, Err(GGUFError::InvalidMagic)));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_parse_unsupported_version() {
        let dir = std::env::temp_dir();
        let path = dir.join("bad_version.gguf");
        let mut buf = b"GGUF".to_vec();
        buf.extend_from_slice(&99u32.to_le_bytes()); // version 99
        buf.extend_from_slice(&0u64.to_le_bytes()); // tensor_count
        buf.extend_from_slice(&0u64.to_le_bytes()); // metadata_kv_count
        let _ = std::fs::write(&path, &buf);
        let result = parse_gguf(&path);
        assert!(matches!(result, Err(GGUFError::UnsupportedVersion(99))));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_parse_minimal_valid_gguf() {
        let dir = std::env::temp_dir();
        let path = dir.join("minimal.gguf");
        let mut buf = b"GGUF".to_vec();
        buf.extend_from_slice(&3u32.to_le_bytes()); // version
        buf.extend_from_slice(&0u64.to_le_bytes()); // tensor_count
        buf.extend_from_slice(&1u64.to_le_bytes()); // metadata_kv_count = 1
        let key = "test.key";
        buf.extend_from_slice(&(key.len() as u64).to_le_bytes());
        buf.extend_from_slice(key.as_bytes());
        buf.extend_from_slice(&4u32.to_le_bytes()); // type = uint32
        buf.extend_from_slice(&42u32.to_le_bytes()); // value
        let _ = std::fs::write(&path, &buf);
        let meta = parse_gguf(&path).unwrap();
        assert_eq!(meta.version, 3);
        assert_eq!(meta.tensor_count, 0);
        assert_eq!(
            meta.raw_metadata.get("test.key").and_then(|v| v.as_u32()),
            Some(42)
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_parse_truncated_metadata() {
        let dir = std::env::temp_dir();
        let path = dir.join("truncated_meta.gguf");
        let mut buf = b"GGUF".to_vec();
        buf.extend_from_slice(&3u32.to_le_bytes()); // version
        buf.extend_from_slice(&0u64.to_le_bytes()); // tensor_count
        buf.extend_from_slice(&1u64.to_le_bytes()); // metadata_kv_count = 1
        buf.extend_from_slice(&10u64.to_le_bytes()); // key length = 10
        buf.extend_from_slice(b"abc"); // only 3 bytes of key, not 10
        let _ = std::fs::write(&path, &buf);
        let result = parse_gguf(&path);
        assert!(result.is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_parse_truncated_tensor_table() {
        let dir = std::env::temp_dir();
        let path = dir.join("truncated_tensors.gguf");
        let mut buf = b"GGUF".to_vec();
        buf.extend_from_slice(&3u32.to_le_bytes()); // version
        buf.extend_from_slice(&1u64.to_le_bytes()); // tensor_count = 1 (but no tensor data follows)
        buf.extend_from_slice(&0u64.to_le_bytes()); // metadata_kv_count = 0
        let _ = std::fs::write(&path, &buf);
        let result = parse_gguf(&path);
        assert!(matches!(result, Err(GGUFError::Io(_))));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_unknown_value_type() {
        let dir = std::env::temp_dir();
        let path = dir.join("unknown_type.gguf");
        let mut buf = b"GGUF".to_vec();
        buf.extend_from_slice(&3u32.to_le_bytes()); // version
        buf.extend_from_slice(&0u64.to_le_bytes()); // tensor_count
        buf.extend_from_slice(&1u64.to_le_bytes()); // metadata_kv_count = 1
        let key = "test";
        buf.extend_from_slice(&(key.len() as u64).to_le_bytes());
        buf.extend_from_slice(key.as_bytes());
        buf.extend_from_slice(&255u32.to_le_bytes()); // type = 255 (unknown)
        let _ = std::fs::write(&path, &buf);
        let result = parse_gguf(&path);
        assert!(matches!(result, Err(GGUFError::InvalidMetadata(_))));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_missing_architecture() {
        let meta = GGUFMetadata {
            version: 3,
            tensor_count: 0,
            architecture: None,
            model_name: None,
            file_type: None,
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
            file_path: "".into(),
            quantisation: None,
            raw_metadata: HashMap::new(),
            tokenizer_model: None,
            chat_template: None,
            bos_token_id: None,
            eos_token_id: None,
            pad_token_id: None,
        };
        let caps = detect_capabilities(&meta);
        assert!(!caps.chat);
        assert!(caps.completion);
        assert!(!caps.fim);
        assert!(!caps.embeddings);
    }

    #[test]
    fn test_missing_context_length_defaults() {
        let meta = GGUFMetadata {
            version: 3,
            tensor_count: 0,
            architecture: None,
            model_name: None,
            file_type: None,
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
            file_path: "".into(),
            quantisation: None,
            raw_metadata: HashMap::new(),
            tokenizer_model: None,
            chat_template: None,
            bos_token_id: None,
            eos_token_id: None,
            pad_token_id: None,
        };
        let est = estimate_memory(&meta);
        assert_eq!(est.kv_context_length, 4096);
    }

    #[test]
    fn test_overflow_safety_large_model() {
        let meta = GGUFMetadata {
            version: 3,
            tensor_count: 0,
            architecture: Some("llama".into()),
            model_name: Some("Giant-1T".into()),
            file_type: Some(15),
            context_length: Some(131072),
            embedding_length: Some(16384),
            block_count: Some(160),
            head_count: Some(128),
            head_dim: Some(128),
            feed_forward_length: None,
            expert_count: None,
            expert_used_count: None,
            parameter_count: Some(1_000_000_000_000.0),
            computed_parameter_count: Some(1_000_000_000_000.0),
            file_size: 500_000_000_000,
            file_path: "".into(),
            quantisation: Some("Q4_K_M".into()),
            raw_metadata: HashMap::new(),
            tokenizer_model: None,
            chat_template: None,
            bos_token_id: None,
            eos_token_id: None,
            pad_token_id: None,
        };
        let est = estimate_memory(&meta);
        assert!(est.model_weights_bytes > 0);
        assert!(est.kv_cache_bytes > 0);
        assert!(est.total_bytes > est.model_weights_bytes);
        assert!(est.recommended_min_ram_bytes <= est.total_bytes);
    }

    fn default_meta() -> GGUFMetadata {
        GGUFMetadata {
            version: 3,
            tensor_count: 0,
            architecture: None,
            model_name: None,
            file_type: None,
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
            file_size: 2_000_000_000,
            file_path: "".into(),
            quantisation: None,
            raw_metadata: HashMap::new(),
            tokenizer_model: None,
            chat_template: None,
            bos_token_id: None,
            eos_token_id: None,
            pad_token_id: None,
        }
    }
}
