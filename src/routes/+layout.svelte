<!--
  Root SvelteKit layout — exists solely to import the global
  stylesheet (`app.css`) so `:root` custom properties land on every
  page (main, /settings, /hud) without each route having to
  re-import. The layout adds no markup of its own; `<slot />`
  passes children through unchanged.
-->
<script lang="ts">
  import "../app.css";

  type Props = {
    children?: import("svelte").Snippet;
  };
  let { children }: Props = $props();
</script>

{@render children?.()}
