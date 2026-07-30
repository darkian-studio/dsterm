#![allow(dead_code)]

pub trait InferenceContext: Send {
    fn n_ctx(&self) -> u32;
}
