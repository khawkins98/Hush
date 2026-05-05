import type { AudioSource, AudioSourceListing } from "$lib/types";

// Audio-source picker state shared by the main dictation controls and
// the meeting panel. `loadSources()` intentionally lives in
// `dictation.svelte.ts` because load failure writes into dictation's
// visible error surface.
let sources = $state<AudioSourceListing[]>([]);
let sourcesLoaded = $state(false);
// Selected source id. Mic devices use their device name; the
// system-audio entry uses the literal string `"system"`. Mapped to
// an `AudioSource` for `start_dictation` in `dictation.start()`.
let selected = $state<string | null>(null);

// Independent state for the meeting panel's source picker.
let meetingMicId = $state<string | null>(null);
let meetingIncludeSystemAudio = $state<boolean>(false);

export const audio = {
  get sources() {
    return sources;
  },
  set sources(val: AudioSourceListing[]) {
    sources = val;
  },
  get sourcesLoaded() {
    return sourcesLoaded;
  },
  set sourcesLoaded(val: boolean) {
    sourcesLoaded = val;
  },
  get selected() {
    return selected;
  },
  set selected(val: string | null) {
    selected = val;
  },
  get meetingMicId() {
    return meetingMicId;
  },
  set meetingMicId(val: string | null) {
    meetingMicId = val;
  },
  get meetingIncludeSystemAudio() {
    return meetingIncludeSystemAudio;
  },
  set meetingIncludeSystemAudio(val: boolean) {
    meetingIncludeSystemAudio = val;
  },
  setSources(list: AudioSourceListing[]) {
    sources = list;
  },
  selectedAsAudioSource(): AudioSource | null {
    if (selected === null) return null;
    if (selected === "system") return { kind: "system-audio" };
    return { kind: "microphone", deviceId: selected };
  },
  findSystemAudio(): AudioSourceListing | undefined {
    return sources.find((s) => s.kind === "system-audio");
  },
};
