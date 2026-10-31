//! Calls a model through `composable:agents/model-client`. For each `chat`
//! invocation, it creates a session with the output schema (if any), passes a
//! request with the prompt, and returns the model response as text.

wit_bindgen::generate!({
    path: "wit",
    world: "model",
    generate_all,
});

use composable::agents::model_client::Session;
use composable::agents::types::{ModelInputItem, ModelOutputItem, ModelRequest, ModelSettings};

struct Model;

impl Guest for Model {
    async fn chat(prompt: String, output_schema: Option<String>) -> Result<String, String> {
        let settings = ModelSettings {
            instructions: None,
            tools: None,
            tool_choice: None,
            output_schema,
            temperature: None,
            top_p: None,
            top_k: None,
            max_tokens: None,
            seed: None,
            stop_sequences: None,
        };
        let session = Session::new(&settings, &[]).map_err(|e| e.message)?;
        let request = ModelRequest {
            input: vec![ModelInputItem::Text(prompt)],
            max_tokens: None,
        };
        let stream = session.call(request).await.map_err(|e| e.message)?;

        // The deltas stream the reply as the model generates it. Reading them
        // to their end allows the call to finish. Dropping would cancel it.
        stream.deltas.collect().await;
        let response = stream.result.await.map_err(|e| e.message)?;
        Ok(response
            .output
            .iter()
            .filter_map(|item| match item {
                ModelOutputItem::Text(text) => Some(text.as_str()),
                _ => None,
            })
            .collect())
    }
}

export!(Model);
