# resources-manager — Windows 服务卸载（管理员权限运行）
#
# 用法：
#   powershell -ExecutionPolicy Bypass -File packaging\windows\uninstall-service.ps1
# 可选 -RemoveData 同时删除数据目录（谨慎：会删本地数据库）。

param(
    [string]$ServiceName = "resources-manager",
    [switch]$RemoveData,
    [string]$DataDir = "$env:ProgramData\resources-manager"
)

$ErrorActionPreference = "Stop"

$nssm = Get-Command nssm -ErrorAction SilentlyContinue
if ($nssm) {
    Write-Host "停止并删除服务 $ServiceName（NSSM）..."
    & nssm stop $ServiceName 2>$null
    & nssm remove $ServiceName confirm
} else {
    Write-Host "停止并删除服务 $ServiceName（sc.exe）..."
    & sc.exe stop $ServiceName 2>$null
    Start-Sleep -Seconds 2
    & sc.exe delete $ServiceName
}

if ($RemoveData) {
    if (Test-Path $DataDir) {
        Write-Warning "删除数据目录: $DataDir"
        Remove-Item -Recurse -Force $DataDir
    }
} else {
    Write-Host "数据目录保留: $DataDir （如需删除请加 -RemoveData）"
}

Write-Host "完成。"
