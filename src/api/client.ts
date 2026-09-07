/**
 * Tauri IPC 封装。所有对 Rust 后端的调用都经由这里，
 * 浏览器(纯 vite dev)下自动降级为 mock，便于独立调试 UI。
 */

import { invoke } from '@tauri-apps/api/core'

export interface HealthInfo {
  app_version: string
  db_path: string | null
  db_ready: boolean
  db_version: number
  message: string
}

export const inTauri = (): boolean =>
  typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window

export async function getHealth(): Promise<HealthInfo> {
  if (!inTauri()) {
    return {
      app_version: '0.1.0 (browser preview)',
      db_path: null,
      db_ready: false,
      db_version: 0,
      message: 'Running in browser preview; Tauri backend not connected.',
    }
  }
  return invoke<HealthInfo>('health')
}
