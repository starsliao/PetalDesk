<script lang="ts">
  import { Check } from "@lucide/svelte";
  import { NOTE_COLOR_OPTIONS, type NoteColor } from "./types";

  interface Props {
    value: NoteColor;
    label?: string;
    onchange?: (color: NoteColor) => void;
  }

  let { value, label = "便签颜色", onchange }: Props = $props();
</script>

<fieldset class="color-picker" aria-label={label}>
  <legend class="sr-only">{label}</legend>
  {#each NOTE_COLOR_OPTIONS as option (option.value)}
    <button
      type="button"
      class="color-swatch"
      class:is-selected={value === option.value}
      style:background={option.hex}
      aria-label={`${option.label}背景`}
      aria-pressed={value === option.value}
      data-color={option.value}
      data-tooltip={option.label}
      title={option.label}
      onclick={() => onchange?.(option.value)}
    >
      {#if value === option.value}
        <Check size={15} strokeWidth={2.4} aria-hidden="true" />
      {/if}
    </button>
  {/each}
</fieldset>

<style>
  .color-picker {
    display: grid;
    grid-template-columns: repeat(7, 28px);
    gap: 7px;
    min-width: 0;
    padding: 0;
    margin: 0;
    border: 0;
  }

  .color-swatch {
    position: relative;
    display: grid;
    width: 28px;
    height: 28px;
    padding: 0;
    place-items: center;
    color: #202020;
    border: 1px solid rgb(0 0 0 / 22%);
    border-radius: 50%;
    box-shadow: inset 0 0 0 1px rgb(255 255 255 / 28%);
    cursor: default;
  }

  .color-swatch:hover {
    border-color: rgb(0 0 0 / 48%);
    transform: translateY(-1px);
  }

  .color-swatch.is-selected {
    border-color: #202020;
    box-shadow: 0 0 0 2px #ffffff, 0 0 0 4px #202020;
  }

  .color-swatch[data-color="charcoal"] {
    color: #ffffff;
  }

  .color-swatch:focus-visible {
    outline-color: var(--app-focus, #005fb8);
  }

  @media (max-width: 280px) {
    .color-picker {
      grid-template-columns: repeat(3, 28px);
    }
  }
</style>
