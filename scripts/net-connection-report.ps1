#Requires -Version 5.1
<#
  飞花 - PetalDesk 网络连接体检(数据口径原型)

  用途:定位连接数异常、临时端口耗尽,以及本地代理环路。
  这是 NetworkTool 小工具的口径原型;Rust 侧改用 GetExtendedTcpTable 实现同一套聚合,
  以便在 2 万条连接下仍能秒级返回(Get-NetTCPConnection 在这个量级会超时返回 0)。

  关键设计:TIME_WAIT 的属主 PID 是 0,按进程聚合看不出是谁造成的。
            所以必须再做一次「按目标端点聚合」,并把回环目标反查成监听方进程,
            才能把 2 万条无主 TIME_WAIT 归因到具体程序。
#>
[CmdletBinding()]
param(
  [int]$Top = 12,
  [int]$LoopMinHits = 100,
  [double]$LoopMinRatio = 0.7
)

$ErrorActionPreference = 'Stop'
try { [Console]::OutputEncoding = [System.Text.Encoding]::UTF8 } catch {}

$LoopbackPattern = '^(127\.|\[::1\]$|::1$)'
$InterestingStates = @('ESTABLISHED', 'TIME_WAIT', 'CLOSE_WAIT', 'FIN_WAIT_1', 'FIN_WAIT_2', 'SYN_SENT', 'LAST_ACK', 'CLOSING', 'LISTENING')

function Split-Endpoint {
  param([string]$Endpoint)
  $i = $Endpoint.LastIndexOf(':')
  if ($i -lt 0) { return [pscustomobject]@{ Address = $Endpoint; Port = 0 } }
  $portText = $Endpoint.Substring($i + 1)
  $port = 0
  [void][int]::TryParse($portText, [ref]$port)
  [pscustomobject]@{ Address = $Endpoint.Substring(0, $i); Port = $port }
}

function Get-Bar {
  param([double]$Ratio, [int]$Width = 22)
  $fill = [int][Math]::Round($Ratio * $Width)
  if ($fill -gt $Width) { $fill = $Width }
  if ($fill -lt 0) { $fill = 0 }
  ('#' * $fill) + ('.' * ($Width - $fill))
}

function Write-Rule {
  param([string]$Title)
  Write-Host ''
  Write-Host ("-- $Title " + ('-' * [Math]::Max(0, 62 - $Title.Length))) -ForegroundColor Cyan
}

# ---------- 采样 ----------
Write-Host ''
Write-Host '飞花 - PetalDesk  网络连接体检' -ForegroundColor White
Write-Host ("采样时间: " + (Get-Date -Format 'yyyy-MM-dd HH:mm:ss')) -ForegroundColor DarkGray

$sw = [System.Diagnostics.Stopwatch]::StartNew()
$raw = & netstat.exe -ano -p TCP 2>$null
$sw.Stop()

$rows = New-Object System.Collections.Generic.List[object]
foreach ($line in $raw) {
  if ($line -notmatch '^\s*TCP\s+(\S+)\s+(\S+)\s+(\S+)\s+(\d+)\s*$') { continue }
  $localText = $Matches[1]
  $remoteText = $Matches[2]
  $state = $Matches[3]
  $owner = [int]$Matches[4]
  $local = Split-Endpoint $localText
  $remote = Split-Endpoint $remoteText
  $rows.Add([pscustomobject]@{
      LocalPort  = $local.Port
      Remote     = $remoteText
      RemoteAddr = $remote.Address
      RemotePort = $remote.Port
      State      = $state
      OwningPid  = $owner
      IsLoopback = ($remote.Address -match $LoopbackPattern)
    })
}

$total = $rows.Count
if ($total -eq 0) {
  Write-Host '未采集到 TCP 连接,netstat 可能被拦截。' -ForegroundColor Red
  return
}
Write-Host ("采样耗时: {0:N0} ms,共 {1:N0} 条 TCP 记录" -f $sw.Elapsed.TotalMilliseconds, $total) -ForegroundColor DarkGray

# 进程名缓存 + 监听端口归属表(把回环目标反查成监听方进程)
$procNames = @{}
foreach ($p in (Get-Process -ErrorAction SilentlyContinue)) { $procNames[$p.Id] = $p.ProcessName }

function Resolve-Proc {
  param([int]$ProcId)
  if ($ProcId -eq 0) { return '(无属主/TIME_WAIT)' }
  if ($ProcId -eq 4) { return 'System' }
  if ($procNames.ContainsKey($ProcId)) { return $procNames[$ProcId] }
  return "(已退出 $ProcId)"
}

$listenerOwner = @{}
foreach ($r in $rows) {
  if ($r.State -eq 'LISTENING' -and -not $listenerOwner.ContainsKey($r.LocalPort)) {
    $listenerOwner[$r.LocalPort] = $r.OwningPid
  }
}

# ---------- 1. 总览 ----------
Write-Rule '1. 连接状态总览'
$byState = $rows | Group-Object State | Sort-Object Count -Descending
foreach ($g in $byState) {
  $ratio = $g.Count / $total
  Write-Host ("  {0,-13} {1,8:N0}  {2,6:N1}%  {3}" -f $g.Name, $g.Count, ($ratio * 100), (Get-Bar $ratio))
}
Write-Host ("  {0,-13} {1,8:N0}" -f '合计', $total) -ForegroundColor White

# ---------- 2. 临时端口占用比 ----------
Write-Rule '2. 临时端口(ephemeral port)占用比'
$dynStart = 49152
$dynCount = 16384
try {
  $netshText = (& netsh.exe int ipv4 show dynamicport tcp 2>$null) -join "`n"
  $nums = @([regex]::Matches($netshText, '\d+') | ForEach-Object { [int]$_.Value })
  if ($nums.Count -ge 2 -and $nums[0] -gt 1024 -and $nums[1] -gt 0) {
    $dynStart = $nums[0]
    $dynCount = $nums[1]
  }
} catch {}
$dynEnd = $dynStart + $dynCount - 1

$usedPorts = @($rows | Where-Object { $_.LocalPort -ge $dynStart -and $_.LocalPort -le $dynEnd } |
    Select-Object -ExpandProperty LocalPort -Unique)
$usedCount = $usedPorts.Count
$portRatio = if ($dynCount -gt 0) { $usedCount / $dynCount } else { 0 }

$verdict, $color = if ($portRatio -ge 0.9) { '危险 - 随时可能任何程序都连不出去', 'Red' }
elseif ($portRatio -ge 0.7) { '注意 - 已接近耗尽', 'Yellow' }
else { '正常', 'Green' }

Write-Host ("  端口范围     {0}-{1}  (共 {2:N0} 个)" -f $dynStart, $dynEnd, $dynCount)
Write-Host ("  已占用       {0:N0}" -f $usedCount)
Write-Host ("  占用比       {0:N1}%   {1}" -f ($portRatio * 100), (Get-Bar $portRatio)) -ForegroundColor $color
Write-Host ("  结论         {0}" -f $verdict) -ForegroundColor $color

# ---------- 3. 按进程聚合 ----------
Write-Rule "3. 按进程聚合(前 $Top)"
Write-Host ("  {0,-7} {1,-22} {2,7} {3,7}  {4}" -f 'PID', '进程', '连接数', '占比', '状态明细') -ForegroundColor DarkGray
$procGroups = $rows | Group-Object OwningPid | Sort-Object Count -Descending | Select-Object -First $Top
foreach ($g in $procGroups) {
  $procId = [int]$g.Name
  $ratio = $g.Count / $total
  $detail = ($g.Group | Group-Object State | Sort-Object Count -Descending |
      ForEach-Object { "{0} {1}" -f $_.Name, $_.Count }) -join '  '
  $line = "  {0,-7} {1,-22} {2,7:N0} {3,6:N1}%  {4}" -f $procId, (Resolve-Proc $procId), $g.Count, ($ratio * 100), $detail
  if ($procId -eq 0) { Write-Host $line -ForegroundColor DarkYellow } else { Write-Host $line }
}
Write-Host '  注:PID 0 是已脱离属主的 TIME_WAIT,按进程归因不到,看下一节。' -ForegroundColor DarkGray

# ---------- 4. 按目标端点聚合(核心) ----------
Write-Rule "4. 按目标端点聚合(前 $Top)-- TIME_WAIT 的真正归因方式"
Write-Host ("  {0,-24} {1,7} {2,7}  {3,-22} {4}" -f '目标端点', '连接数', '占比', '该端点归属进程', '状态明细') -ForegroundColor DarkGray
$outbound = @($rows | Where-Object { $_.State -ne 'LISTENING' })
$outboundTotal = [Math]::Max(1, $outbound.Count)
$targetGroups = $outbound | Group-Object Remote | Sort-Object Count -Descending | Select-Object -First $Top
foreach ($g in $targetGroups) {
  $sample = $g.Group[0]
  $ratio = $g.Count / $outboundTotal
  $ownerText = '-'
  if ($sample.IsLoopback -and $listenerOwner.ContainsKey($sample.RemotePort)) {
    $ownerPid = $listenerOwner[$sample.RemotePort]
    $ownerText = "{0} ({1})" -f (Resolve-Proc $ownerPid), $ownerPid
  }
  elseif ($sample.IsLoopback) {
    $ownerText = '(本机,监听已关闭)'
  }
  $detail = ($g.Group | Group-Object State | Sort-Object Count -Descending |
      Select-Object -First 3 | ForEach-Object { "{0} {1}" -f $_.Name, $_.Count }) -join '  '
  $line = "  {0,-24} {1,7:N0} {2,6:N1}%  {3,-22} {4}" -f $g.Name, $g.Count, ($ratio * 100), $ownerText, $detail
  if ($ratio -ge 0.1) { Write-Host $line -ForegroundColor Yellow } else { Write-Host $line }
}

# ---------- 5. 环路研判 ----------
Write-Rule '5. 本地代理环路研判'
# 思路:某个回环监听端点收到大量连接,而它的属主进程自己的出站又几乎全部回到本机
#       -> 说明流量在「A 转给 B、B 又被转回 A」之间空转。
$loopFindings = New-Object System.Collections.Generic.List[object]
$loopbackTargets = $outbound | Where-Object { $_.IsLoopback } | Group-Object Remote |
  Where-Object { $_.Count -ge $LoopMinHits } | Sort-Object Count -Descending

foreach ($g in $loopbackTargets) {
  $sample = $g.Group[0]
  if (-not $listenerOwner.ContainsKey($sample.RemotePort)) { continue }
  $ownerPid = $listenerOwner[$sample.RemotePort]
  if ($ownerPid -le 4) { continue }

  $ownerRows = @($outbound | Where-Object { $_.OwningPid -eq $ownerPid })
  if ($ownerRows.Count -eq 0) { continue }
  $ownerLoop = @($ownerRows | Where-Object { $_.IsLoopback }).Count
  $ownerExt = $ownerRows.Count - $ownerLoop
  $ownerRatio = $ownerLoop / $ownerRows.Count
  if ($ownerRatio -lt $LoopMinRatio) { continue }

  $loopFindings.Add([pscustomobject]@{
      Target     = $g.Name
      Hits       = $g.Count
      OwnerPid   = $ownerPid
      OwnerName  = (Resolve-Proc $ownerPid)
      OwnerLoop  = $ownerLoop
      OwnerExt   = $ownerExt
      OwnerRatio = $ownerRatio
    })
}

if ($loopFindings.Count -eq 0) {
  Write-Host '  未发现明显的本地代理环路。' -ForegroundColor Green
}
else {
  foreach ($f in $loopFindings) {
    Write-Host ("  [环路嫌疑] {0}" -f $f.Target) -ForegroundColor Red
    Write-Host ("     该端点被打入   {0:N0} 条连接,归属 {1} (PID {2})" -f $f.Hits, $f.OwnerName, $f.OwnerPid)
    Write-Host ("     而 {0} 自身出站 {1:N0} 条回环 / {2:N0} 条真实外网 = {3:N1}% 在原地空转" -f `
        $f.OwnerName, $f.OwnerLoop, $f.OwnerExt, ($f.OwnerRatio * 100)) -ForegroundColor Red
    # 谁在往这个端点打
    $callers = $outbound | Where-Object { $_.Remote -eq $f.Target -and $_.OwningPid -gt 0 } |
      Group-Object OwningPid | Sort-Object Count -Descending | Select-Object -First 5
    foreach ($c in $callers) {
      Write-Host ("       <- {0,-20} {1,6:N0} 条" -f (Resolve-Proc ([int]$c.Name)), $c.Count) -ForegroundColor DarkGray
    }
    Write-Host ''
  }
}

Write-Host ''
