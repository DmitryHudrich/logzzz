import { LayoutGrid, Search } from 'lucide-react'

import type { GraphLayoutMode } from '@/shared/graph'
import { Badge } from '@/shared/ui/badge'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/shared/ui/card'
import { Input } from '@/shared/ui/input'
import { ToggleGroup, ToggleGroupItem } from '@/shared/ui/toggle-group'

type TransactionGraphControlsProps = {
  query: string
  onQueryChange: (value: string) => void
  layout: GraphLayoutMode
  onLayoutChange: (value: GraphLayoutMode) => void
  nodeCount: number
  edgeCount: number
  selectedNodeLabel?: string | null
}

export const TransactionGraphControls = ({
  query,
  onQueryChange,
  layout,
  onLayoutChange,
  nodeCount,
  edgeCount,
  selectedNodeLabel,
}: TransactionGraphControlsProps) => {
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
          <div className='flex items-center gap-2 text-xs font-medium text-muted-foreground'>
            <LayoutGrid className='size-3.5' />
            Layout
          </div>
          <ToggleGroup
            type='single'
            variant='outline'
            size='sm'
            value={layout}
            onValueChange={(value) => {
              if (value === 'force' || value === 'concentric' || value === 'breadthfirst') {
                onLayoutChange(value)
              }
            }}
            className='grid w-full grid-cols-3'
          >
            <ToggleGroupItem value='force' className='w-full'>
              Force
            </ToggleGroupItem>
            <ToggleGroupItem value='concentric' className='w-full'>
              Concentric
            </ToggleGroupItem>
            <ToggleGroupItem value='breadthfirst' className='w-full'>
              Flow
            </ToggleGroupItem>
          </ToggleGroup>
        </div>

        <div className='flex flex-wrap gap-2'>
          <Badge variant='secondary'>nodes: {nodeCount}</Badge>
          <Badge variant='secondary'>edges: {edgeCount}</Badge>
          {selectedNodeLabel ? <Badge variant='outline'>selected: {selectedNodeLabel}</Badge> : null}
        </div>
      </CardContent>
    </Card>
  )
}
