# Flaborful RunPod Training Worker

Serverless worker that runs Axolotl LoRA fine-tuning on RunPod GPUs and uploads the resulting adapter to Cloudflare R2.

## Deploy once

```bash
# Build and push to Docker Hub (or any registry)
docker build -t yourdockerhubuser/flaborful-trainer:latest .
docker push yourdockerhubuser/flaborful-trainer:latest
```

Then in the RunPod dashboard:
1. Serverless → New Endpoint
2. Container image: `yourdockerhubuser/flaborful-trainer:latest`
3. GPU: A40 or A100 (48GB+ VRAM recommended for Qwen2.5-7B QLoRA)
4. Min workers: 0, Max workers: 5 (scales to zero when idle — you pay nothing)
5. Copy the Endpoint ID → set as `RUNPOD_ENDPOINT_ID` in your `.env`

## What it does

1. Receives dataset (instruction/output pairs) inline from the Rust backend
2. Writes Axolotl config YAML targeting Qwen2.5-Coder-7B-Instruct with QLoRA (4-bit, rank 16)
3. Runs `axolotl train` (~30–60 min on A40)
4. Uploads `adapter_model.safetensors` + `adapter_config.json` to Cloudflare R2
5. Returns `{ agent_id, adapter_key }` — RunPod POSTs this to your webhook

## Cost estimate

- A40 48GB: ~$0.44/hr on RunPod spot
- Training time: ~30–60 min per professional
- Cost per professional: ~$0.25–$0.45 one-time
