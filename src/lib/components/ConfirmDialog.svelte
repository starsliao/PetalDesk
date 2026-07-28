<script lang="ts">
  import { X } from "@lucide/svelte";

  interface Props {
    open?: boolean;
    title: string;
    detail?: string;
    confirmLabel?: string;
    cancelLabel?: string;
    tone?: "danger" | "primary";
    busy?: boolean;
    onconfirm?: () => void | Promise<void>;
    oncancel?: () => void;
  }

  let {
    open = false,
    title,
    detail = "",
    confirmLabel = "确定",
    cancelLabel = "取消",
    tone = "danger",
    busy = false,
    onconfirm,
    oncancel,
  }: Props = $props();

  let confirming = $state(false);
  let submitting = $derived(busy || confirming);

  function cancel(): void {
    if (!submitting) oncancel?.();
  }

  async function confirm(): Promise<void> {
    if (submitting) return;
    confirming = true;
    try {
      await onconfirm?.();
    } finally {
      confirming = false;
    }
  }

  function handleKeydown(event: KeyboardEvent): void {
    if (open && event.key === "Escape") {
      event.preventDefault();
      cancel();
    }
  }
</script>

<svelte:window onkeydown={handleKeydown} />

{#if open}
  <div class="dialog-backdrop">
    <button
      type="button"
      class="backdrop-dismiss"
      aria-label="关闭确认框"
      disabled={submitting}
      onclick={cancel}
    ></button>
    <div class="confirm-dialog" role="alertdialog" aria-modal="true" aria-labelledby="confirm-dialog-title">
      <div class="dialog-heading">
        <h2 id="confirm-dialog-title">{title}</h2>
        <button type="button" class="icon-button" aria-label="关闭" disabled={submitting} onclick={cancel}>
          <X size={17} aria-hidden="true" />
        </button>
      </div>
      {#if detail}
        <p>{detail}</p>
      {/if}
      <div class="dialog-actions">
        <button type="button" class="secondary-button" disabled={submitting} onclick={cancel}>{cancelLabel}</button>
        <button
          type="button"
          class:danger-button={tone === "danger"}
          class:primary-button={tone === "primary"}
          disabled={submitting}
          onclick={() => void confirm()}
        >
          {submitting ? "处理中…" : confirmLabel}
        </button>
      </div>
    </div>
  </div>
{/if}

<style>
  .dialog-backdrop {
    position: fixed;
    z-index: 1000;
    inset: 0;
    display: grid;
    padding: 18px;
    place-items: center;
    background: rgb(0 0 0 / 28%);
  }

  .backdrop-dismiss {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    padding: 0;
    background: transparent;
    border: 0;
  }

  .confirm-dialog {
    position: relative;
    width: min(100%, 390px);
    padding: 18px;
    color: var(--app-fg, #202020);
    background: var(--app-surface, #ffffff);
    border: 1px solid var(--app-border, #d8d8d8);
    border-radius: 7px;
    box-shadow: var(--shadow-flyout, 0 8px 24px rgb(0 0 0 / 16%));
  }

  .dialog-heading,
  .dialog-actions {
    display: flex;
    min-width: 0;
    align-items: center;
  }

  .dialog-heading {
    align-items: flex-start;
    justify-content: space-between;
    gap: 16px;
  }

  h2,
  p {
    margin: 0;
  }

  h2 {
    padding-top: 4px;
    font-size: 16px;
    font-weight: 650;
    line-height: 1.35;
  }

  .confirm-dialog > p {
    margin-top: 10px;
    color: var(--app-muted, #686868);
    font-size: 13px;
    line-height: 1.5;
  }

  .dialog-actions {
    margin-top: 20px;
    justify-content: flex-end;
    gap: 8px;
  }

  .secondary-button,
  .danger-button,
  .primary-button {
    display: inline-flex;
    min-height: 32px;
    padding: 5px 11px;
    align-items: center;
    justify-content: center;
    gap: 6px;
    font-size: 12.5px;
    font-weight: 600;
    border-radius: 4px;
    cursor: default;
  }

  .secondary-button {
    color: var(--app-fg, #202020);
    background: #ffffff;
    border: 1px solid var(--app-border-strong, #b9b9b9);
  }

  .danger-button {
    color: #ffffff;
    background: var(--app-danger, #c42b1c);
    border: 1px solid #a7281b;
  }

  .primary-button {
    color: #ffffff;
    background: var(--app-accent, #0067c0);
    border: 1px solid #00589f;
  }

  .secondary-button:hover {
    background: #f5f5f5;
  }

  .danger-button:hover {
    background: #ab2619;
  }

  .primary-button:hover {
    background: #005da9;
  }

  .secondary-button:disabled,
  .danger-button:disabled,
  .primary-button:disabled {
    cursor: not-allowed;
    opacity: 0.62;
  }

  @media (max-width: 420px) {
    .dialog-actions {
      align-items: stretch;
      flex-direction: column-reverse;
    }

    .dialog-actions button {
      width: 100%;
    }
  }
</style>
