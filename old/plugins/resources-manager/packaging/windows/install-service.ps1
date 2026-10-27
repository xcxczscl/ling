# resources-manager — Windows 服务安装（PowerShell，管理员权限运行）
#
# 用法（在解压后的发行包根目录）：
#   powershell -ExecutionPolicy Bypass -File packaging\windows\install-service.ps1
#
# 依赖：NSSM（Non-Sucking Service Manager）或 sc.exe。
# 本脚本优先用 nssm；找不到时回退 sc.exe（无自动重启，仅基础注册）。
#
# 服务名：resources-manager
# 账户：LocalSystem（可按需改为指定用户）

param(
    [string]$InstallDir = "$PSScriptRoot\..\..",
    [string]$ServiceName = "resources-manager",
    [string]$DataDir = "$env:ProgramData\resources-manager"
)

$ErrorActionPreference = "Stop"
$lingExe = Join-Path $InstallDir "bin\ling.exe"
if (-not (Test-Path $lingExe)) {
    # 也接受无 .exe 后缀（交叉编译产物命名差异）
    $lingExe = Join-Path $InstallDir "bin\ling"
}
if (-not (Test-Path $lingExe)) {
    Write-Error "找不到 ling 可执行文件：$lingExe"
}

New-Item -ItemType Directory -Force -Path $DataDir | Out-Null
Write-Host "安装目录: $InstallDir"
Write-Host "数据目录: $DataDir"
Write-Host "服务可执行: $lingExe"

$nssm = Get-Command nssm -ErrorAction SilentlyContinue
if ($nssm) {
    Write-Host "使用 NSSM 注册服务..."
    & nssm install $ServiceName $lingExe
    & nssm set $ServiceName AppDirectory $DataDir
    & nssm set $ServiceName AppEnvironmentExtra "LING_DATA_DIR=$DataDir"
    & nssm set $ServiceName Start SERVICE_AUTO_START
    & nssm set $ServiceName AppExit Default Restart
    & nssm set $ServiceName AppRestartDelay 2000
    & nssm set $ServiceName DisplayName "ling 资源管理宿主"
    & nssm set $ServiceName Description "本机回环资源管理服务（127.0.0.1:7910）"
    & nssm start $ServiceName
    Write-Host "服务 $ServiceName 已注册并启动（开机自启）。"
} else {
    Write-Host "未找到 nssm，回退 sc.exe（无自动重启）..."
    & sc.exe create $ServiceName binPath= "\"$lingExe\"" start= auto
    & sc.exe description $ServiceName "ling 资源管理宿主（本机回环 7910）"
    & sc.exe start $ServiceName
    Write-Warning "sc.exe 模式无自动重启；生产环境建议安装 NSSM 后重跑本脚本。"
}

Write-Host ""
Write-Host "验证："
Write-Host "  sc query $ServiceName"
Write-Host "  或浏览器访问 http://127.0.0.1:7910 （口令见 $DataDir\ling\session.json）"
