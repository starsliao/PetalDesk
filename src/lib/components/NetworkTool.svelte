<script lang="ts">
  import { Activity, AlertTriangle, LoaderCircle, RefreshCw, X } from "@lucide/svelte";
  import {
    barWidth,
    ephemeralVerdict,
    formatCount,
    formatRatio,
    headline,
    networkApi,
    stateSummary,
    targetOwnerLabel,
    unownedShare,
    type NetworkApi,
    type NetworkReport,
  } from "../network";

  interface Props {
    api?: NetworkApi;
  }

  let { api = networkApi }: Props = $props();

  let report = $state<NetworkReport | null>(null);
  let loading = $state(false);
  let error = $state("");

  let severity = $derived.by(() => {
    if (!report) return "idle";
    if (report.loops.length > 0 || report.ephemeral.level === "critical") return "critical";
    if (report.ephemeral.level === "warning") return "warning";
    return "normal";
  });

  async function collect(): Promise<void> {
    if (loading) return;
    loading = true;
    error = "";
    try {
      report = await api.collect();
    } catch (reason) {
      error = reason instanceof Error ? reason.message : "网络体检失败，请稍后重试。";
    } finally {
      loading = false;
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

<div class="network-tool">
  <header class="titlebar" data-tauri-drag-region>
    <div class="brand" data-tauri-drag-region>
      <Activity size={18} aria-hidden="true" />
      <h1 data-tauri-drag-region>网络体检</h1>
    </div>
    <div class="window-actions">
      <button
        type="button"
        class="primary-icon"
        aria-label="重新查询"
        title="重新查询"
        disabled={loading}
        onclick={() => void collect()}
      >
        {#if loading}
          <LoaderCircle class="spinner" size={18} aria-hidden="true" />
        {:else}
          <RefreshCw size={18} aria-hidden="true" />
        {/if}
      </button>
      <button type="button" aria-label="关闭网络体检窗口" title="关闭" onclick={() => void closeWindow()}>
        <X size={18} aria-hidden="true" />
      </button>
    </div>
  </header>

  <main>
    {#if error}
      <div class="error-banner" role="alert">{error}</div>
    {/if}

    {#if !report}
      <div class="empty-state">
        <Activity size={26} aria-hidden="true" />
        <strong>还没有采集数据</strong>
        <span>
          查询会枚举本机全部 TCP 连接，按状态、进程和目标端点聚合，并研判本地代理环路。只读取连接元数据，不读取任何流量内容。
        </span>
        <button type="button" class="primary-button" disabled={loading} onclick={() => void collect()}>
          {#if loading}
            <LoaderCircle class="spinner" size={16} aria-hidden="true" />
            正在采集…
          {:else}
            <RefreshCw size={16} aria-hidden="true" />
            开始查询
          {/if}
        </button>
      </div>
    {:else}
      <p class="sample-line">
        采样于 {report.sampledAt} · 耗时 {formatCount(report.sampleMs)} ms · 共 {formatCount(report.total)} 条 TCP 记录
      </p>

      <section class="verdict {severity}" aria-label="体检结论">
        {#if severity === "critical" || severity === "warning"}
          <AlertTriangle size={18} aria-hidden="true" />
        {/if}
        <p>{headline(report)}</p>
      </section>

      <section class="panel" aria-label="连接状态总览">
        <h2>连接状态总览</h2>
        <ul class="bars">
          {#each report.states as bucket (bucket.state)}
            <li>
              <span class="bar-label">{bucket.state}</span>
              <span class="bar-track"><span class="bar-fill" style="width: {barWidth(bucket.ratio)}"></span></span>
              <span class="bar-value">{formatCount(bucket.count)}</span>
              <span class="bar-pct">{formatRatio(bucket.ratio)}</span>
            </li>
          {/each}
        </ul>
      </section>

      <section class="panel" aria-label="临时端口占用比">
        <h2>临时端口占用比</h2>
        <div class="ephemeral {report.ephemeral.level}">
          <div class="ephemeral-figure">
            <strong>{formatRatio(report.ephemeral.ratio)}</strong>
            <span>
              {formatCount(report.ephemeral.used)} / {formatCount(report.ephemeral.capacity)} 个端口
            </span>
          </div>
          <div class="ephemeral-detail">
            <span class="bar-track tall">
              <span class="bar-fill" style="width: {barWidth(report.ephemeral.ratio)}"></span>
            </span>
            <p class="range">动态端口范围 {report.ephemeral.start}-{report.ephemeral.end}</p>
            <p class="verdict-text">{ephemeralVerdict(report.ephemeral)}</p>
          </div>
        </div>
      </section>

      <section class="panel" aria-label="按进程聚合">
        <h2>按进程聚合</h2>
        <table>
          <thead>
            <tr>
              <th scope="col">PID</th>
              <th scope="col">进程</th>
              <th scope="col" class="num">连接数</th>
              <th scope="col" class="num">占比</th>
              <th scope="col">状态明细</th>
            </tr>
          </thead>
          <tbody>
            {#each report.processes as bucket (bucket.pid)}
              <tr class:unowned={bucket.pid === 0}>
                <td class="num">{bucket.pid}</td>
                <td class="name">{bucket.name}</td>
                <td class="num">{formatCount(bucket.count)}</td>
                <td class="num">{formatRatio(bucket.ratio)}</td>
                <td class="detail">{stateSummary(bucket.states, 4)}</td>
              </tr>
            {/each}
          </tbody>
        </table>
        {#if unownedShare(report) > 0}
          <p class="note">
            PID 0 是已脱离属主的 TIME_WAIT，占了 {formatRatio(unownedShare(report))}，按进程归因不到 ——
            要看下面的目标聚合。
          </p>
        {/if}
      </section>

      <section class="panel" aria-label="按目标端点聚合">
        <h2>按目标端点聚合</h2>
        <p class="panel-hint">把回环目标的端口反查成监听方进程，无主的 TIME_WAIT 就能落到具体程序头上。</p>
        <table>
          <thead>
            <tr>
              <th scope="col">目标端点</th>
              <th scope="col" class="num">连接数</th>
              <th scope="col" class="num">占比</th>
              <th scope="col">该端点归属进程</th>
              <th scope="col">状态明细</th>
            </tr>
          </thead>
          <tbody>
            {#each report.targets as target (target.endpoint)}
              <tr class:hot={target.ratio >= 0.1}>
                <td class="name">{target.endpoint}</td>
                <td class="num">{formatCount(target.count)}</td>
                <td class="num">{formatRatio(target.ratio)}</td>
                <td class="name">{targetOwnerLabel(target)}</td>
                <td class="detail">{stateSummary(target.states)}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      </section>

      <section class="panel" aria-label="本地代理环路研判">
        <h2>本地代理环路研判</h2>
        {#if report.loops.length === 0}
          <p class="ok-text">未发现明显的本地代理环路。</p>
        {:else}
          {#each report.loops as finding (finding.endpoint)}
            <article class="loop-card">
              <h3><AlertTriangle size={16} aria-hidden="true" /> {finding.endpoint}</h3>
              <p>
                该端点被打入 <strong>{formatCount(finding.hits)}</strong> 条连接，归属
                <strong>{finding.ownerName}</strong>（PID {finding.ownerPid}）。
              </p>
              <p>
                而 {finding.ownerName} 自身出站 {formatCount(finding.ownerLoopbackOut)} 条回环 /
                {formatCount(finding.ownerExternalOut)} 条真实外网 ——
                <strong>{formatRatio(finding.ownerLoopbackRatio)} 在原地空转</strong>。
              </p>
              {#if finding.callers.length > 0}
                <ul class="callers">
                  {#each finding.callers as caller (caller.pid)}
                    <li><span>{caller.name}</span><em>{formatCount(caller.count)} 条</em></li>
                  {/each}
                </ul>
              {/if}
            </article>
          {/each}
        {/if}
      </section>
    {/if}
  </main>
</div>

<style>
  .network-tool { display: grid; width: 100%; height: 100%; min-width: 0; min-height: 0; grid-template-rows: 44px minmax(0, 1fr); color: #202020; background: #f3f3f3; }
  .titlebar { display: flex; min-width: 0; align-items: center; justify-content: space-between; padding: 5px 6px 5px 12px; background: rgb(250 250 250 / 96%); border-bottom: 1px solid #d7d7d7; user-select: none; }
  .brand { display: flex; min-width: 0; align-items: center; gap: 8px; color: #5b4b89; }
  h1 { margin: 0; color: #252525; font-size: 14px; font-weight: 650; }
  .window-actions { display: flex; align-items: center; gap: 2px; }
  button { font: inherit; }
  .window-actions button { display: grid; width: 32px; height: 32px; padding: 0; place-items: center; color: #555; background: transparent; border: 0; border-radius: 5px; cursor: pointer; }
  .window-actions button:hover { color: #202020; background: #e7e7e7; }
  .window-actions button:disabled { cursor: default; opacity: .5; }
  .window-actions .primary-icon { color: #5c3fa3; }
  .window-actions button:last-child:hover { color: #fff; background: #c42b1c; }
  main { min-width: 0; min-height: 0; padding: 16px; overflow: auto; }
  .error-banner { margin-bottom: 12px; padding: 9px 11px; color: #8c1d14; font-size: 12.5px; line-height: 1.4; background: #fff0ed; border: 1px solid #f1c1bb; border-radius: 6px; }
  .empty-state { display: flex; min-height: 260px; padding: 28px; align-items: center; justify-content: center; flex-direction: column; gap: 9px; color: #777; text-align: center; background: #fff; border: 1px dashed #c7c7c7; border-radius: 8px; }
  .empty-state strong { color: #333; font-size: 14px; }
  .empty-state span { max-width: 420px; font-size: 12px; line-height: 1.5; }
  .primary-button { display: flex; min-height: 32px; margin-top: 4px; align-items: center; justify-content: center; gap: 6px; padding: 5px 12px; color: #fff; background: #6845ad; border: 1px solid #573697; border-radius: 5px; cursor: pointer; }
  .primary-button:hover { background: #5a399b; }
  .primary-button:disabled { cursor: default; opacity: .6; }
  .sample-line { margin: 0 0 12px; color: #6f6f6f; font-size: 11.5px; }
  .verdict { display: flex; align-items: flex-start; gap: 9px; margin-bottom: 14px; padding: 11px 13px; font-size: 12.5px; line-height: 1.5; border-radius: 7px; }
  .verdict p { margin: 0; }
  .verdict.normal { color: #1c5b32; background: #eefaf1; border: 1px solid #b8e2c5; }
  .verdict.warning { color: #7a4b06; background: #fff7e8; border: 1px solid #f0d49b; }
  .verdict.critical { color: #8c1d14; background: #fff0ed; border: 1px solid #f1c1bb; }
  .panel { margin-bottom: 14px; padding: 14px 15px 15px; background: #fff; border: 1px solid #d8d8d8; border-radius: 8px; }
  h2 { margin: 0 0 11px; color: #262626; font-size: 13.5px; font-weight: 650; }
  .panel-hint { margin: -6px 0 11px; color: #6f6f6f; font-size: 11.5px; line-height: 1.45; }
  .bars { display: grid; gap: 6px; margin: 0; padding: 0; list-style: none; }
  .bars li { display: grid; align-items: center; grid-template-columns: 92px minmax(0, 1fr) 68px 54px; gap: 9px; font-size: 12px; }
  .bar-label { color: #4a4a4a; font-variant-numeric: tabular-nums; }
  .bar-track { display: block; height: 8px; overflow: hidden; background: #eceaf3; border-radius: 999px; }
  .bar-track.tall { height: 12px; }
  .bar-fill { display: block; height: 100%; background: #7352b9; border-radius: 999px; }
  .bar-value, .bar-pct { color: #444; font-variant-numeric: tabular-nums; text-align: right; }
  .bar-pct { color: #7a7a7a; }
  .ephemeral { display: grid; align-items: center; grid-template-columns: minmax(0, 150px) minmax(0, 1fr); gap: 16px; }
  .ephemeral-figure { display: grid; gap: 3px; }
  .ephemeral-figure strong { font-size: 30px; font-weight: 650; font-variant-numeric: tabular-nums; line-height: 1.1; }
  .ephemeral-figure span { color: #6f6f6f; font-size: 11.5px; }
  .ephemeral-detail p { margin: 8px 0 0; font-size: 11.5px; }
  .range { color: #6f6f6f; }
  .verdict-text { font-weight: 600; }
  .ephemeral.normal .ephemeral-figure strong, .ephemeral.normal .verdict-text { color: #1c5b32; }
  .ephemeral.warning .ephemeral-figure strong, .ephemeral.warning .verdict-text { color: #a2660c; }
  .ephemeral.critical .ephemeral-figure strong, .ephemeral.critical .verdict-text { color: #c42b1c; }
  .ephemeral.warning .bar-fill { background: #e0961f; }
  .ephemeral.critical .bar-fill { background: #c42b1c; }
  table { width: 100%; border-collapse: collapse; font-size: 12px; }
  th, td { padding: 6px 8px; text-align: left; border-bottom: 1px solid #eee; }
  th { color: #6f6f6f; font-size: 11px; font-weight: 600; }
  tbody tr:last-child td { border-bottom: 0; }
  .num { text-align: right; font-variant-numeric: tabular-nums; white-space: nowrap; }
  .name { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .detail { color: #6f6f6f; font-size: 11px; overflow-wrap: anywhere; }
  tr.unowned td { color: #9a6b12; background: #fffaf0; }
  tr.hot td { color: #8c1d14; background: #fff6f4; font-weight: 600; }
  .note { margin: 10px 0 0; padding: 8px 10px; color: #7a4b06; font-size: 11.5px; line-height: 1.45; background: #fff7e8; border-radius: 6px; }
  .ok-text { margin: 0; color: #1c5b32; font-size: 12.5px; }
  .loop-card { padding: 12px 13px; margin-bottom: 9px; color: #6f2f28; background: #fff6f4; border: 1px solid #f1c1bb; border-radius: 7px; }
  .loop-card:last-child { margin-bottom: 0; }
  .loop-card h3 { display: flex; align-items: center; gap: 6px; margin: 0 0 8px; color: #8c1d14; font-size: 13px; font-variant-numeric: tabular-nums; }
  .loop-card p { margin: 0 0 6px; font-size: 12.5px; line-height: 1.55; }
  .callers { display: grid; gap: 3px; margin: 9px 0 0; padding: 9px 0 0; list-style: none; border-top: 1px dashed #e3b4ad; }
  .callers li { display: flex; justify-content: space-between; gap: 12px; font-size: 11.5px; }
  .callers em { font-style: normal; font-variant-numeric: tabular-nums; }
  :global(.spinner) { animation: spin 850ms linear infinite; }
  @keyframes spin { to { transform: rotate(360deg); } }
  @media (max-width: 620px) { main { padding: 12px; } .ephemeral { grid-template-columns: 1fr; gap: 10px; } .bars li { grid-template-columns: 84px minmax(0, 1fr) 58px 48px; gap: 7px; } }
</style>
