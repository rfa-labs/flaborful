"""
RunPod serverless worker — Axolotl LoRA training for Flaborful.

Input (from Rust backend):
{
  "input": {
    "agent_id": "uuid-string",
    "dataset": [{"instruction": "...", "output": "..."}],
    "base_model": "Qwen/Qwen2.5-Coder-7B-Instruct",
    "lora_rank": 16,
    "r2_bucket": "flaborful-lora",
    "r2_endpoint": "https://<account>.r2.cloudflarestorage.com",
    "r2_access_key": "...",
    "r2_secret_key": "...",
    "webhook_url": "https://api.flaborful.com/api/v1/onboarding/lora/complete"
  }
}

Output (POSTed back to webhook by RunPod):
{
  "output": {
    "agent_id": "uuid-string",
    "adapter_key": "adapters/{agent_id}/adapter_model.safetensors"
  }
}
"""

import json
import os
import subprocess
import tempfile
import textwrap
import boto3
import runpod


def handler(job):
    inp = job["input"]

    agent_id   = inp["agent_id"]
    dataset    = inp["dataset"]
    base_model = inp.get("base_model", "mistralai/Mistral-7B-Instruct-v0.2")
    lora_rank  = inp.get("lora_rank", 16)
    r2_bucket  = inp["r2_bucket"]
    r2_endpoint = inp["r2_endpoint"]
    r2_access_key = inp["r2_access_key"]
    r2_secret_key = inp["r2_secret_key"]

    with tempfile.TemporaryDirectory() as workdir:
        dataset_path = os.path.join(workdir, "dataset.jsonl")
        config_path  = os.path.join(workdir, "config.yml")
        output_dir   = os.path.join(workdir, "output")
        os.makedirs(output_dir, exist_ok=True)

        # Write dataset as alpaca-format JSONL
        with open(dataset_path, "w") as f:
            for pair in dataset:
                f.write(json.dumps({
                    "instruction": pair["instruction"],
                    "input": "",
                    "output": pair["output"],
                }) + "\n")

        # Write Axolotl config
        config = textwrap.dedent(f"""
            base_model: {base_model}
            model_type: AutoModelForCausalLM
            tokenizer_type: AutoTokenizer

            load_in_8bit: false
            load_in_4bit: true
            strict: false

            datasets:
              - path: {dataset_path}
                type: alpaca
                data_files:
                  - {dataset_path}
                split: train

            dataset_prepared_path: {workdir}/prepared
            val_set_size: 0.05
            output_dir: {output_dir}

            sequence_len: 2048
            sample_packing: false
            pad_to_sequence_len: false

            adapter: lora
            lora_r: {lora_rank}
            lora_alpha: {lora_rank * 2}
            lora_dropout: 0.05
            lora_target_linear: true

            gradient_accumulation_steps: 4
            micro_batch_size: 2
            num_epochs: 3
            optimizer: adamw_bnb_8bit
            lr_scheduler: cosine
            learning_rate: 2e-4

            train_on_inputs: false
            group_by_length: false
            bf16: auto
            fp16: false
            tf32: false

            gradient_checkpointing: true
            logging_steps: 10
            flash_attention: false

            warmup_steps: 10
            saves_per_epoch: 1
            save_safetensors: true
        """).strip()

        with open(config_path, "w") as f:
            f.write(config)

        # Run Axolotl training
        result = subprocess.run(
            f"axolotl train {config_path}",
            shell=True,
            capture_output=True,
            text=True,
        )

        if result.returncode != 0:
            return {"error": f"Axolotl training failed:\n{result.stderr[-3000:]}"}

        # Find the adapter files
        safetensors = os.path.join(output_dir, "adapter_model.safetensors")
        adapter_cfg = os.path.join(output_dir, "adapter_config.json")

        if not os.path.exists(safetensors):
            return {"error": "adapter_model.safetensors not found after training"}

        # Upload to R2
        s3 = boto3.client(
            "s3",
            endpoint_url=r2_endpoint,
            aws_access_key_id=r2_access_key,
            aws_secret_access_key=r2_secret_key,
            region_name="auto",
        )

        safetensors_key = f"adapters/{agent_id}/adapter_model.safetensors"
        config_key      = f"adapters/{agent_id}/adapter_config.json"

        s3.upload_file(safetensors, r2_bucket, safetensors_key)

        if os.path.exists(adapter_cfg):
            s3.upload_file(adapter_cfg, r2_bucket, config_key)

    return {
        "agent_id": agent_id,
        "adapter_key": safetensors_key,
    }


runpod.serverless.start({"handler": handler})
