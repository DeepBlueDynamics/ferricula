#[cfg(feature = "ml")]
#[test]
#[ignore = "requires local ONNX model artifacts via SHIVVR_MODEL_PATH and SHIVVR_TOKENIZER_PATH"]
fn smoke_test_existing_gtr_t5_model() {
    let model_path = std::env::var("SHIVVR_MODEL_PATH")
        .expect("SHIVVR_MODEL_PATH environment variable must be set to run smoke_test_existing_gtr_t5_model");
    let tokenizer_path = std::env::var("SHIVVR_TOKENIZER_PATH")
        .expect("SHIVVR_TOKENIZER_PATH environment variable must be set to run smoke_test_existing_gtr_t5_model");

    assert!(
        std::path::Path::new(&model_path).exists(),
        "Model file does not exist at {model_path}"
    );
    assert!(
        std::path::Path::new(&tokenizer_path).exists(),
        "Tokenizer file does not exist at {tokenizer_path}"
    );

    let embedder = ferricula_semantic::embedder::Embedder::new(&model_path, &tokenizer_path)
        .expect("Failed to create Embedder from GTR-T5 ONNX model");

    let vector = embedder.embed("Steve Jobs typography Macintosh")
        .expect("Failed to embed text with GTR-T5 model");

    assert_eq!(vector.len(), 768, "Expected 768-dimensional embedding from GTR-T5-base");
    let norm = vector.iter().map(|x| x * x).sum::<f32>().sqrt();
    println!("SUCCESS: Embedded text into {}-dimensional vector. Vector norm: {:.4}", vector.len(), norm);
}
