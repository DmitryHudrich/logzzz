import { ToggleGroup, ToggleGroupItem } from '@/shared/ui/toggle-group'

export type GraphSourceMode = 'mock' | 'backend'

type TransactionGraphSourceToggleProps = {
  value: GraphSourceMode
  onChange: (value: GraphSourceMode) => void
}

export const TransactionGraphSourceToggle = ({ value, onChange }: TransactionGraphSourceToggleProps) => {
  return (
    <div className='flex flex-col gap-2'>
      <span className='text-xs font-medium text-muted-foreground'>Data source</span>
      <ToggleGroup
        type='single'
        variant='outline'
        size='sm'
        value={value}
        onValueChange={(next) => {
          if (next === 'mock' || next === 'backend') {
            onChange(next)
          }
        }}
        className='grid w-full grid-cols-2'
      >
        <ToggleGroupItem value='mock' aria-label='Mock data source' className='w-full'>
          Mock
        </ToggleGroupItem>
        <ToggleGroupItem value='backend' aria-label='Backend data source' className='w-full'>
          Backend
        </ToggleGroupItem>
      </ToggleGroup>
    </div>
  )
}
