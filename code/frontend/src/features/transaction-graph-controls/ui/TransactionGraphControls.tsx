import { LayoutGrid, Search } from 'lucide-react'

import { GRAPH_LAYOUT_OPTIONS, isGraphLayoutMode } from '@/shared/graph'
import type { GraphLayoutMode } from '@/shared/graph'
import { Badge } from '@/shared/ui/badge'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/shared/ui/card'
import { Input } from '@/shared/ui/input'
import { RangeTimelineSlider } from '@/shared/ui/range-timeline-slider'
import { ToggleGroup, ToggleGroupItem } from '@/shared/ui/toggle-group'

type TransactionGraphControlsProps = {
  query: string
  onQueryChange: (value: string) => void
  layout: GraphLayoutMode
  onLayoutChange: (value: GraphLayoutMode) => void
  nodeCount: number
  edgeCount: number
  selectedNodeLabel?: string | null
  blockRange?: {
    from: number
    to: number
  } | null
  selectedBlockRange?: {
    from: number
    to: number
  } | null
  onBlockRangeChange?: (range: { from: number; to: number }) => void
}

export const TransactionGraphControls = ({
  query,
  onQueryChange,
  layout,
  onLayoutChange,
  nodeCount,
  edgeCount,
  selectedNodeLabel,
  blockRange,
  selectedBlockRange,
  onBlockRangeChange,
}: TransactionGraphControlsProps) => {
  const activeLayout = GRAPH_LAYOUT_OPTIONS.find((option) => option.value === layout)

  return (
    <Card className='gap-4 py-4'>
      <CardHeader className='px-4 pb-0'>
        <CardTitle className='text-base'>Graph controls</CardTitle>
        <CardDescription>Filter visible nodes and switch layout mode.</CardDescription>
      </CardHeader>

      <CardContent className='space-y-4 px-4'>
        <div className='relative'>
          <Search className='pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-muted-foreground' />
          <Input
            value={query}
            onChange={(event) => onQueryChange(event.target.value)}
            placeholder='Filter by address, amount or symbol'
            className='pl-9'
          />
        </div>

        <div className='space-y-2'>
          <div className='flex items-center justify-between gap-2 text-xs font-medium text-muted-foreground'>
            <div className='inline-flex items-center gap-2'>
              <LayoutGrid className='size-3.5' />
              Layout
            </div>
            {activeLayout ? <span className='text-[11px] font-normal'>{activeLayout.description}</span> : null}
          </div>
          <ToggleGroup
            type='single'
            variant='outline'
            size='sm'
            value={layout}
            onValueChange={(value) => {
              if (isGraphLayoutMode(value)) {
                onLayoutChange(value)
              }
            }}
            className='flex w-full flex-nowrap overflow-x-auto'
          >
            {GRAPH_LAYOUT_OPTIONS.map((option) => (
              <ToggleGroupItem value={option.value} className='shrink-0 px-2 text-xs'>
                {option.label}
              </ToggleGroupItem>
            ))}
          </ToggleGroup>
        </div>

        <div className='flex flex-wrap gap-2'>
          <Badge variant='secondary'>nodes: {nodeCount}</Badge>
          <Badge variant='secondary'>edges: {edgeCount}</Badge>
          {selectedNodeLabel ? <Badge variant='outline'>selected: {selectedNodeLabel}</Badge> : null}
        </div>

        <div className='space-y-2 rounded-md border border-border/70 bg-card/40 p-3'>
          <div className='text-xs font-medium text-muted-foreground'>Block timeline</div>
          {blockRange ? (
            <div className='space-y-2'>
              {selectedBlockRange && onBlockRangeChange ? (
                <div className='space-y-1.5 rounded border border-border/60 bg-background/50 px-2 py-1.5'>
                  <div className='font-mono text-[11px] text-muted-foreground'>
                    {selectedBlockRange.from.toLocaleString()} - {selectedBlockRange.to.toLocaleString()}
                  </div>
                  <RangeTimelineSlider
                    min={blockRange.from}
                    max={blockRange.to}
                    value={selectedBlockRange}
                    onChange={onBlockRangeChange}
                  />
                </div>
              ) : null}
              <div className='flex items-center justify-between font-mono text-[11px] text-muted-foreground'>
                <span>{blockRange.from.toLocaleString()}</span>
                <span>{blockRange.to.toLocaleString()}</span>
              </div>
            </div>
          ) : (
            <p className='text-xs text-muted-foreground'>No block data for current graph.</p>
          )}
        </div>
      </CardContent>
    </Card>
  )
}
