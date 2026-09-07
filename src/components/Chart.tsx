import { useEffect, useRef } from 'react'
import * as echarts from 'echarts'
import type { EChartsOption } from 'echarts'

export interface ChartProps {
  option: EChartsOption
  height?: number
  /** 是否跟随系统明暗主题切换背景色(默认透明) */
  transparent?: boolean
  onEvents?: {
    click?: (params: unknown) => void
  }
}

/** ECharts 轻量封装: 自动 init/dispose/resize */
export default function Chart({ option, height = 300, transparent = true, onEvents }: ChartProps) {
  const ref = useRef<HTMLDivElement | null>(null)
  const chartRef = useRef<echarts.ECharts | null>(null)

  useEffect(() => {
    if (!ref.current) return
    const chart = echarts.init(ref.current)
    chartRef.current = chart
    const onResize = () => chart.resize()
    window.addEventListener('resize', onResize)

    return () => {
      window.removeEventListener('resize', onResize)
      chart.dispose()
      chartRef.current = null
    }
  }, [])

  useEffect(() => {
    const chart = chartRef.current
    if (!chart) return
    chart.setOption(option, { notMerge: true })
    // 事件重建
    if (onEvents?.click) {
      chart.off('click')
      chart.on('click', (params) => onEvents.click?.(params))
    }
  }, [option, onEvents])

  return (
    <div
      ref={ref}
      style={{ width: '100%', height, background: transparent ? 'transparent' : undefined }}
    />
  )
}
