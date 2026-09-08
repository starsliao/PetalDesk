<script lang="ts">
  import { AlertTriangle, LoaderCircle, Radar, X } from "@lucide/svelte";
  import {
    PROBE_KINDS,
    formatMs,
    formatPercent,
    headline,
    needsPort,
    netCheckApi,
    outcomeLabel,
    portError,
    verdictLabel,
    verdictTone,
    type NetCheckApi,
    type NetCheckReport,
    type ProbeKind,
  } from "../netcheck";

  interface Props {
    api?: NetCheckApi;
  }

  let { api = netCheckApi }: Props = $props();

  let target = $state("");
  let kind = $state<ProbeKind>("ping");
  let port = $state("");
  let count = $state(4);
  let timeoutMs = $state(2000);

  let report = $state<NetCheckReport | null>(null);
  let running = $state(false);
  let error = $state("");

  let showPort = $derived(needsPort(kind));
  let validationError = $derived.by(() => {
    if (target.trim().length === 0) return "请输入要检测的地址。";
    return portError(kind, port);
  });
  let canRun = $derived(!running && validationError === null);

  async function run(): Promise<void> {
    if (running) return;
    const problem = validationError;
    if (problem) {
      error = problem;
      return;
    }
    running = true;
    error = "";
    try {
      report = await api.run({
        target: target.trim(),
        kind,
        port: showPort ? Number(port.trim()) : null,
        count,
        timeoutMs,
      });
    } catch (reason) {
      error = reason instanceof Error ? reason.message : "网络检测失败，请稍后重试。";
    } finally {
      running = false;
    }
  }

  async function closeWindow(): Promise<void> {
    if (!api.isDesktop()) return;
    try {
      const { getCurrentWindow } = await import("@tauri-apps/api/window");
      const appWindow = getCurrentWindow();
      try {
        await appWindow.close();
      } catch {
        await appWindow.destroy();
      }
    } catch {
      // Browser preview has no native window to close.
    }
  }
</script>

<div class="netcheck-tool">
  <header class="titlebar" data-tauri-drag-region>
    <div class="brand" data-tauri-drag-region>
      <Radar size={18} aria-hidden="true" />
      <h1 data-tauri-drag-region>网络检测</h1>
    </div>
    <div class="window-actions">
      <button type="button" aria-label="关闭网络检测窗口" title="关闭" onclick={() => void closeWindow()}>
        <X size={18} aria-hidden="true" />
      </button>
    </div>
  </header>

  <main>
    <form
      class="probe-form"
      onsubmit={(event) => {
        event.preventDefault();
        void run();
      }}
    >
      <div class="field-row">
        <label class="grow">
          <span>地址</span>
          <input
            bind:value={target}
            placeholder="域名或 IP，例如 github.com 或 10.0.0.1"
            autocomplete="off"
            spellcheck="false"
          />
        </label>
        <label class="kind-field">
          <span>方式</span>
          <select bind:value={kind}>
            {#each PROBE_KINDS as option (option.value)}
              <option value={option.value}>{option.label}</option>
            {/each}
          </select>
        </label>
        {#if showPort}
          <label class="port-field">
            <span>端口</span>
            <input bind:value={port} placeholder="443" inputmode="numeric" autocomplete="off" />
          </label>
        {/if}
      </div>

      <div class="field-row">
        <label class="small-field">
          <span>次数</span>
          <input type="number" min="1" max="20" bind:value={count} />
        </label>
        <label class="small-field">
          <span>超时 (ms)</span>
          <input type="number" min="100" max="10000" step="100" bind:value={timeoutMs} />
        </label>
        <button type="submit" class="primary-button" disabled={!canRun}>
          {#if running}
            <LoaderCircle class="spinner" size={16} aria-hidden="true" />
            正在检测…
          {:else}
            <Radar size={16} aria-hidden="true" />
            开始检测
          {/if}
        </button>
      </div>
    </form>

    {#if error}
      <div class="error-banner" role="alert">{error}</div>
    {/if}

    {#if report}
      <section class="verdict {verdictTone(report.verdict)}" aria-label="检测结论">
        {#if verdictTone(report.verdict) !== "good"}
          <AlertTriangle size={18} aria-hidden="true" />
        {/if}
        <div>
          <strong>{verdictLabel(report.verdict)}</strong>
          <p>{headline(report)}</p>
        </div>
      </section>

      {#if report.probeNote}
        <p class="probe-note">{report.probeNote}</p>
      {/if}

      <section class="stats" aria-label="延时统计">
        <div class="stat">
          <span>最小</span>
          <strong>{formatMs(report.latency?.minMs ?? null)}</strong>
        </div>
        <div class="stat">
          <span>平均</span>
          <strong>{formatMs(report.latency?.avgMs ?? null)}</strong>
        </div>
        <div class="stat">
          <span>最大</span>
          <strong>{formatMs(report.latency?.maxMs ?? null)}</strong>
        </div>
        <div class="stat">
          <span>抖动</span>
          <strong>{formatMs(report.latency?.jitterMs ?? null)}</strong>
        </div>
        <div class="stat">
          <span>丢包</span>
          <strong>{formatPercent(report.lossRatio)}</strong>
        </div>
      </section>

      <p class="sample-line">
        {report.target} → {report.resolvedIp}{report.port === null ? "" : `:${report.port}`} ·
        {report.startedAt} · 耗时 {report.elapsedMs} ms
      </p>

      <section class="panel" aria-label="逐次结果">
        <table>
          <thead>
            <tr>
              <th scope="col">#</th>
              <th scope="col">结果</th>
              <th scope="col" class="num">延时</th>
              <th scope="col">说明</th>
            </tr>
          </thead>
          <tbody>
            {#each report.attempts as attempt (attempt.seq)}
              <tr class={attempt.outcome}>
                <td class="num">{attempt.seq}</td>
                <td class="outcome">{outcomeLabel(attempt.outcome)}</td>
                <td class="num">{formatMs(attempt.latencyMs)}</td>
                <td class="detail">{attempt.detail}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      </section>
    {:else if !running}
      <div class="empty-state">
        <Radar size={26} aria-hidden="true" />
        <strong>输入地址后开始检测</strong>
        <span>
          Ping 使用系统 ICMP 接口，不需要管理员权限。TCP 通过完整握手判断端口是否开放。UDP
          没有握手，因此只会给出「有应答」「明确关闭」「无法确定」三种结论，不会把测不准说成不通。
        </span>
      </div>
    {/if}
  </main>
</div>

<style>
  .netcheck-tool { display: grid; width: 100%; height: 100%; min-width: 0; min-height: 0; grid-template-rows: 44px minmax(0, 1fr); color: #202020; background: #f3f3f3; }
  .titlebar { display: flex; min-width: 0; align-items: center; justify-content: space-between; padding: 5px 6px 5px 12px; background: rgb(250 250 250 / 96%); border-bottom: 1px solid #d7d7d7; user-select: none; }
  .brand { display: flex; min-width: 0; align-items: center; gap: 8px; color: #5b4b89; }
  h1 { margin: 0; color: #252525; font-size: 14px; font-weight: 650; }
  .window-actions { display: flex; align-items: center; gap: 2px; }
  button { font: inherit; }
  .window-actions button { display: grid; width: 32px; height: 32px; padding: 0; place-items: center; color: #555; background: transparent; border: 0; border-radius: 5px; cursor: pointer; }
  .window-actions button:hover { color: #fff; background: #c42b1c; }
  main { min-width: 0; min-height: 0; padding: 16px; overflow: auto; }
  .probe-form { padding: 13px 14px 14px; margin-bottom: 13px; background: #fff; border: 1px solid #d8d8d8; border-radius: 8px; }
  .field-row { display: flex; align-items: flex-end; gap: 10px; }
  .field-row + .field-row { margin-top: 10px; }
  label { display: grid; min-width: 0; gap: 5px; color: #4f4f4f; font-size: 12px; }
  .grow { flex: 1 1 auto; }
  .kind-field { flex: 0 0 110px; }
  .port-field { flex: 0 0 92px; }
  .small-field { flex: 0 0 96px; }
  input, select { width: 100%; min-width: 0; height: 34px; padding: 0 9px; color: #202020; font: inherit; background: #fff; border: 1px solid #b8b8b8; border-radius: 5px; outline: 0; }
  input:focus, select:focus { border-color: #7352b9; box-shadow: inset 0 -2px #7352b9; }
  .primary-button { display: flex; height: 34px; margin-left: auto; align-items: center; justify-content: center; gap: 6px; padding: 0 16px; color: #fff; background: #6845ad; border: 1px solid #573697; border-radius: 5px; cursor: pointer; }
  .primary-button:hover { background: #5a399b; }
  .primary-button:disabled { cursor: default; opacity: .6; }
  .error-banner { margin-bottom: 12px; padding: 9px 11px; color: #8c1d14; font-size: 12.5px; line-height: 1.4; background: #fff0ed; border: 1px solid #f1c1bb; border-radius: 6px; }
  .empty-state { display: flex; min-height: 200px; padding: 24px; align-items: center; justify-content: center; flex-direction: column; gap: 9px; color: #777; text-align: center; background: #fff; border: 1px dashed #c7c7c7; border-radius: 8px; }
  .empty-state strong { color: #333; font-size: 14px; }
  .empty-state span { max-width: 440px; font-size: 12px; line-height: 1.6; }
  .verdict { display: flex; align-items: flex-start; gap: 9px; margin-bottom: 12px; padding: 11px 13px; border-radius: 7px; }
  .verdict strong { display: block; margin-bottom: 3px; font-size: 13px; }
  .verdict p { margin: 0; font-size: 12.5px; line-height: 1.55; }
  .verdict.good { color: #1c5b32; background: #eefaf1; border: 1px solid #b8e2c5; }
  .verdict.warning { color: #7a4b06; background: #fff7e8; border: 1px solid #f0d49b; }
  .verdict.bad { color: #8c1d14; background: #fff0ed; border: 1px solid #f1c1bb; }
  .probe-note { margin: -6px 0 12px; padding: 8px 10px; color: #5d5a66; font-size: 11.5px; line-height: 1.5; background: #f4f2f8; border-radius: 6px; }
  .stats { display: grid; grid-template-columns: repeat(5, minmax(0, 1fr)); gap: 8px; margin-bottom: 11px; }
  .stat { display: grid; gap: 3px; padding: 9px 10px; text-align: center; background: #fff; border: 1px solid #d8d8d8; border-radius: 7px; }
  .stat span { color: #79747f; font-size: 10.5px; }
  .stat strong { color: #362552; font-size: 14px; font-variant-numeric: tabular-nums; }
  .sample-line { margin: 0 0 11px; color: #6f6f6f; font-size: 11.5px; overflow-wrap: anywhere; }
  .panel { padding: 12px 14px 13px; background: #fff; border: 1px solid #d8d8d8; border-radius: 8px; }
  table { width: 100%; border-collapse: collapse; font-size: 12px; }
  th, td { padding: 6px 8px; text-align: left; border-bottom: 1px solid #eee; }
  th { color: #6f6f6f; font-size: 11px; font-weight: 600; }
  tbody tr:last-child td { border-bottom: 0; }
  .num { width: 34px; text-align: right; font-variant-numeric: tabular-nums; white-space: nowrap; }
  .outcome { white-space: nowrap; font-weight: 600; }
  tr.open .outcome { color: #1c5b32; }
  tr.closed .outcome { color: #8c1d14; }
  tr.filtered .outcome { color: #a2660c; }
  tr.unreachable .outcome, tr.error .outcome { color: #8c1d14; }
  .detail { color: #6f6f6f; font-size: 11px; overflow-wrap: anywhere; }
  :global(.spinner) { animation: spin 850ms linear infinite; }
  @keyframes spin { to { transform: rotate(360deg); } }
  @media (max-width: 560px) {
    main { padding: 12px; }
    .field-row { flex-wrap: wrap; }
    .grow { flex: 1 1 100%; }
    .stats { grid-template-columns: repeat(3, minmax(0, 1fr)); }
  }
</style>
