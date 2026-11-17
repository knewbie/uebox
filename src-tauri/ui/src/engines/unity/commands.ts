import { invoke } from '@tauri-apps/api/core'

export interface UnityInspection {
  project: {
    projectRoot: string
    projectName: string
    unityVersion: string
    assetsPath: string
    platform: string
    renderPipeline: string
  }
  scene: {
    name: string
    path: string
    buildIndex: number
    isDirty: boolean
    isLoaded: boolean
    rootCount: number
  }
}

export function inspectUnity(
  projectRoot: string,
  port: number,
  allowClientTakeover: boolean
): Promise<UnityInspection> {
  return invoke<UnityInspection>('inspect_unity', { projectRoot, port, allowClientTakeover })
}
