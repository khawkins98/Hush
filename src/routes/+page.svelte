<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";

  // Mirror the camelCase serde renames on the Rust side.
  type AudioDevice = { id: string; name: string; isDefault: boolean };
  type ForegroundApp = { appName: string; windowTitle: string };
  type DictationResult = { text: string; foreground: ForegroundApp | null };
  type IpcError = { kind: string; message?: string };

  let devices = $state<AudioDevice[]>([]);
  let selected = $state<string | null>(null);
  let recording = $state(false);
  let busy = $state(false);
  let result = $state<DictationResult | null>(null);
  let error = $state<string | null>(null);

  onMount(async () => {
    try {
      devices = await invoke<AudioDevice[]>("list_input_devices");
      const def = devices.find((d) => d.isDefault) ?? devices[0];
      if (def) selected = def.id;
    } catch (e) {
      error = formatError(e);
    }
  });

  async function start() {
    error = null;
    result = null;
    busy = true;
    try {
      await invoke("start_dictation", { deviceId: selected });
      recording = true;
    } catch (e) {
      error = formatError(e);
    } finally {
      busy = false;
    }
  }

  async function stop() {
    busy = true;
    try {
      result = await invoke<DictationResult>("stop_dictation");
      recording = false;
    } catch (e) {
      error = formatError(e);
      // Even if transcription failed, the recording itself stopped on the
      // Rust side — surface that so the UI is never stuck in "recording".
      recording = false;
    } finally {
      busy = false;
    }
  }

  function formatError(e: unknown): string {
    if (typeof e === "object" && e !== null && "kind" in e) {
      const ipc = e as IpcError;
      return ipc.message ? `${ipc.kind}: ${ipc.message}` : ipc.kind;
    }
    return String(e);
  }
</script>

<main class="container">
  <h1>Hush</h1>
  <p class="tagline">Press, talk, paste. Local Whisper transcription.</p>

  <section class="controls">
    <label>
      Input device
      <select bind:value={selected} disabled={recording || busy}>
        {#each devices as device (device.id)}
          <option value={device.id}>
            {device.name}{device.isDefault ? " (default)" : ""}
          </option>
        {/each}
      </select>
    </label>

    {#if !recording}
      <button onclick={start} disabled={busy || devices.length === 0}>
        ● Start recording
      </button>
    {:else}
      <button class="stop" onclick={stop} disabled={busy}>
        ■ Stop and transcribe
      </button>
    {/if}
  </section>

  {#if error}
    <p class="error">{error}</p>
  {/if}

  {#if result}
    <section class="result">
      <h2>Transcription</h2>
      <p class="text">{result.text || "(empty)"}</p>
      {#if result.foreground}
        <p class="meta">
          Captured while focused on <em>{result.foreground.appName}</em>
          {#if result.foreground.windowTitle}— {result.foreground.windowTitle}{/if}
        </p>
      {/if}
      <p class="meta">Already on your clipboard. Paste with ⌘V / Ctrl+V.</p>
    </section>
  {/if}
</main>

<style>
:root {
  font-family: Inter, Avenir, Helvetica, Arial, sans-serif;
  font-size: 16px;
  line-height: 24px;
  color: #0f0f0f;
  background-color: #f6f6f6;
  font-synthesis: none;
  text-rendering: optimizeLegibility;
  -webkit-font-smoothing: antialiased;
  -moz-osx-font-smoothing: grayscale;
}

.container {
  max-width: 36rem;
  margin: 0 auto;
  padding: 4vh 1.5rem;
  text-align: center;
}

h1 {
  margin: 0 0 0.25rem;
  font-size: 2.5rem;
  letter-spacing: -0.02em;
}

.tagline {
  color: #555;
  margin: 0 0 2rem;
}

.controls {
  display: flex;
  flex-direction: column;
  gap: 1rem;
  align-items: stretch;
}

label {
  display: flex;
  flex-direction: column;
  gap: 0.35rem;
  text-align: left;
  font-size: 0.85rem;
  color: #555;
}

select,
button {
  border-radius: 8px;
  border: 1px solid #d1d1d1;
  padding: 0.7em 1.2em;
  font-size: 1em;
  font-family: inherit;
  color: #0f0f0f;
  background-color: #ffffff;
  transition: border-color 0.15s, background-color 0.15s;
}

button {
  cursor: pointer;
  font-weight: 600;
}

button:hover:not(:disabled) {
  border-color: #396cd8;
}

button:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

button.stop {
  background-color: #d83a3a;
  color: white;
  border-color: #d83a3a;
}

.error {
  margin-top: 1.5rem;
  padding: 0.75rem 1rem;
  background-color: #fee;
  border: 1px solid #d83a3a;
  border-radius: 8px;
  color: #b03030;
  text-align: left;
}

.result {
  margin-top: 2rem;
  padding: 1rem 1.25rem;
  background-color: white;
  border: 1px solid #d1d1d1;
  border-radius: 12px;
  text-align: left;
}

.result h2 {
  margin: 0 0 0.5rem;
  font-size: 1rem;
  color: #555;
  font-weight: 600;
}

.result .text {
  margin: 0 0 0.75rem;
  font-size: 1.1rem;
  line-height: 1.5;
  white-space: pre-wrap;
}

.result .meta {
  margin: 0.25rem 0;
  font-size: 0.85rem;
  color: #666;
}

@media (prefers-color-scheme: dark) {
  :root {
    color: #f0f0f0;
    background-color: #1a1a1a;
  }
  .tagline,
  label,
  .result h2,
  .result .meta {
    color: #aaa;
  }
  select,
  button {
    color: #f0f0f0;
    background-color: #2a2a2a;
    border-color: #3a3a3a;
  }
  button:hover:not(:disabled) {
    border-color: #6a8cf0;
  }
  .result {
    background-color: #2a2a2a;
    border-color: #3a3a3a;
  }
  .error {
    background-color: #3a1a1a;
    color: #ffa0a0;
  }
}
</style>
