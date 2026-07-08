import Slider from 'rc-slider'
import 'rc-slider/assets/index.css'

import { cn } from '@/shared/lib/cn'

import './range-timeline-slider.css'

type RangeValue = {
  from: number
  to: number
}

type RangeTimelineSliderProps = {
  min: number
  max: number
  value: RangeValue
  onChange: (value: RangeValue) => void
  className?: string
  minSpan?: number
}

function clampRange(min: number, max: number, from: number, to: number, minSpan: number): RangeValue {
  let nextFrom = Math.max(min, Math.min(from, max))
  let nextTo = Math.max(min, Math.min(to, max))

  if (nextFrom > nextTo) {
    ;[nextFrom, nextTo] = [nextTo, nextFrom]
  }

  if (nextTo - nextFrom < minSpan) {
    if (nextFrom + minSpan <= max) {
      nextTo = nextFrom + minSpan
    } else {
      nextFrom = Math.max(min, nextTo - minSpan)
    }
  }

  return { from: nextFrom, to: nextTo }
}

export function RangeTimelineSlider({
  min,
  max,
  value,
  onChange,
  className,
  minSpan = 0,
}: RangeTimelineSliderProps) {
  return (
    <div className={cn('range-timeline-slider px-0.5', className)}>
      <Slider
        range={{ draggableTrack: true }}
        min={min}
        max={max}
        value={[value.from, value.to]}
        pushable={minSpan > 0 ? minSpan : false}
        onChange={(nextValue) => {
          const [from, to] = nextValue as number[]
          onChange(clampRange(min, max, from, to, minSpan))
        }}
      />
    </div>
  )
}
