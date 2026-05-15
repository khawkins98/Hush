<!--
  Root SvelteKit layout — imports the global stylesheet (`app.css`)
  so the fixed brand palette applies to every window.
-->
<script lang="ts">
  import "../app.css";

  type Props = {
    children?: import("svelte").Snippet;
  };
  let { children }: Props = $props();
</script>

{@render children?.()}
