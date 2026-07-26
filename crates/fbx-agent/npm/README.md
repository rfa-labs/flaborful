# @rfa-labs/fbx-agent

flaborful's DAG-aware coding orchestrator. It runs **inside a Sprite VM**, drives a bounded
reason→tool→observe loop, and routes every LLM call through the RFA Labs optimizer edge. It is
installed on the sprite the same way the other coding runtimes are (`@anthropic-ai/claude-code`,
`@openai/codex`):

```
npm install -g @rfa-labs/fbx-agent
fbx-agent step --config /workspace/.fbx/step.json
```

This package ships a prebuilt **linux-x64** binary (Sprites are Debian/glibc x86-64); it is not
meant to run on a developer laptop. The engine that invokes it is `flaborful`'s `FbxAgentRuntime`.

See the feature note **Flaborful fbx-agent** in the RFA Labs vault for architecture + the
suspend/resume protocol.
