//! Precomputes CLIP text embeddings for the Documents categories into
//! `src/people/doc_prompts.json`, so the app only needs CLIP's image half.
//!
//! Needs `text_model_quantized.onnx` and `tokenizer.json` from Xenova/clip-vit-base-patch32
//! (Hugging Face) in assets/clip:
//!   cargo run --release --example make_doc_prompts

use tract_onnx::prelude::*;

/// (category, prompts). `None` = everyday photos, which a picture must beat to count as a document.
const PROMPTS: &[(Option<&str>, &[&str])] = &[
    (Some("receipt"), &["a photo of a receipt", "a printed store receipt with prices", "a bill or invoice", "a cash register receipt"]),
    (Some("notes"), &["handwritten notes on paper", "a page of a notebook with handwriting", "a whiteboard with writing", "a printed document page full of text"]),
    (Some("social"), &["a screenshot of a social media post", "a meme with text", "a screenshot of a chat conversation", "a screenshot of an instagram post"]),
    (Some("recipe"), &["a restaurant menu", "a recipe with a list of ingredients", "a page from a cookbook", "a food menu with prices"]),
    (Some("identity"), &["a plastic identity card", "an open passport showing the data page", "a driver's license card", "an ID card with printed text fields and a small photo"]),
    (None, &[
        "a photo of a person", "a selfie", "a group of people", "a landscape", "a photo of food on a plate",
        "a photo of a pet", "a city street", "a room interior", "a party", "a car", "flowers", "a building",
        "a beach", "a child playing", "a wedding", "a sunset", "a colorful abstract gradient", "a television test pattern",
        "a formal portrait photo of a person", "a professional headshot of a person in a suit", "a close-up photo of a face",
        "an official portrait in front of a flag",
    ]),
];

fn main() -> TractResult<()> {
    let tok = tokenizers::Tokenizer::from_file("assets/clip/tokenizer.json").map_err(|e| TractError::msg(e.to_string()))?;
    let model = tract_onnx::onnx()
        .with_ignore_output_shapes(true)
        .with_ignore_value_info(true)
        .model_for_path("assets/clip/text_model_quantized.onnx")?;

    let mut out = Vec::new();
    for (cat, prompts) in PROMPTS {
        for p in *prompts {
            let ids: Vec<i64> = tok.encode(*p, true).map_err(|e| TractError::msg(e.to_string()))?.get_ids().iter().map(|&i| i as i64).collect();
            let plan = model.clone().with_input_fact(0, i64::fact([1, ids.len()]).into())?.into_optimized()?.into_runnable()?;
            let input = tract_ndarray::Array2::from_shape_vec((1, ids.len()), ids)?;
            let emb = plan.run(tvec!(Tensor::from(input).into()))?;
            let v = emb[0].try_as_plain_ram()?.as_slice::<f32>()?.to_vec();
            let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            let v: Vec<f32> = v.iter().map(|x| (x / n * 1e5).round() / 1e5).collect();
            println!("{:>9} {p}", cat.unwrap_or("-"));
            out.push(serde_json::json!({ "category": cat, "prompt": p, "embedding": v }));
        }
    }
    std::fs::write("src/people/doc_prompts.json", serde_json::to_string(&out)?)?;
    println!("wrote src/people/doc_prompts.json");
    Ok(())
}
