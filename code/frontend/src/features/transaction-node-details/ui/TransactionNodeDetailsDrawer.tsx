import { motion } from 'framer-motion'
import { Copy, Loader2, X } from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { toast } from 'sonner'

import { ENTITY_CATEGORIES, SANCTION_LISTS, type AddressLabel, type EntityCategory, type SanctionList } from '@/entities/label'
import type { TransactionEdge, TransactionNodeDetails } from '@/entities/transaction'
import { Accordion, AccordionContent, AccordionItem, AccordionTrigger } from '@/shared/ui/accordion'
import { Badge } from '@/shared/ui/badge'
import { Button } from '@/shared/ui/button'
import { copyToClipboard } from '@/shared/lib/copy-to-clipboard'
import { cn } from '@/shared/lib/cn'
import { Input } from '@/shared/ui/input'
import { Label } from '@/shared/ui/label'
import { motionContainer, motionItem } from '@/shared/ui/motion'
import { RangeTimelineSlider } from '@/shared/ui/range-timeline-slider'
import { ScrollArea } from '@/shared/ui/scroll-area'
import { Separator } from '@/shared/ui/separator'
import { Skeleton } from '@/shared/ui/skeleton'
import { ToggleGroup, ToggleGroupItem } from '@/shared/ui/toggle-group'

type TransactionNodeDetailsDrawerProps = {
  open: boolean
  onOpenChange: (open: boolean) => void
  details: TransactionNodeDetails | null
  isLoading?: boolean
  onAddOriginFromNode?: (params: {
    maxDepth: number
    maxNodes: number
    mode: 'fetch' | 'draw'
  }) => Promise<void> | void
  defaultMaxDepth?: number
  defaultMaxNodes?: number
  isAddingOrigin?: boolean
  label?: AddressLabel | null
  onSaveLabel?: (payload: {
    category: EntityCategory | string
    labelName: string
    sanctionList: SanctionList | string | null
  }) => Promise<void> | void
  isSavingLabel?: boolean
}

const groupLabels: Record<string, string> = {
  wallet: 'Wallet',
  exchange: 'Exchange',
  risk: 'Risk',
}

export const TransactionNodeDetailsDrawer = ({
  open,
  onOpenChange,
  details,
  isLoading = false,
  onAddOriginFromNode,
  defaultMaxDepth = 2,
  defaultMaxNodes = 500,
  isAddingOrigin = false,
  label = null,
  onSaveLabel,
  isSavingLabel = false,
}: TransactionNodeDetailsDrawerProps) => {
  useEffect(() => {
    if (!open) {
      return
    }

    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        onOpenChange(false)
      }
    }

    window.addEventListener('keydown', onKeyDown)
    return () => {
      window.removeEventListener('keydown', onKeyDown)
    }
  }, [open, onOpenChange])

  return (
    <aside
      aria-hidden={!open}
      className={cn(
        'fixed inset-y-0 right-0 z-50 flex w-full max-w-md flex-col border-l border-border bg-background shadow-xl transition-transform duration-300 ease-out',
        open ? 'translate-x-0' : 'pointer-events-none translate-x-full',
      )}
    >
      <div className='flex items-center justify-end border-b border-border px-3 py-2'>
        <Button type='button' variant='ghost' size='icon' aria-label='Close node details' onClick={() => onOpenChange(false)}>
          <X />
        </Button>
      </div>

      <div className='flex min-h-0 flex-1 flex-col'>
        {isLoading ? (
          <PanelLoadingState />
        ) : details ? (
          <PanelDetailsContent
            details={details}
            onAddOriginFromNode={onAddOriginFromNode}
            defaultMaxDepth={defaultMaxDepth}
            defaultMaxNodes={defaultMaxNodes}
            isAddingOrigin={isAddingOrigin}
            label={label}
            onSaveLabel={onSaveLabel}
            isSavingLabel={isSavingLabel}
          />
        ) : (
          <div className='space-y-1.5 p-4'>
            <h2 className='font-semibold text-foreground'>Node details</h2>
            <p className='text-sm text-muted-foreground'>Select a node on the graph to inspect its transactions.</p>
          </div>
        )}
      </div>
    </aside>
  )
}

function PanelLoadingState() {
  return (
    <div className='space-y-4 p-4'>
      <Skeleton className='h-6 w-40' />
      <Skeleton className='h-4 w-full' />
      <div className='grid grid-cols-2 gap-3'>
        <Skeleton className='h-16' />
        <Skeleton className='h-16' />
        <Skeleton className='h-16' />
        <Skeleton className='h-16' />
      </div>
      <Skeleton className='h-24' />
      <Skeleton className='h-24' />
    </div>
  )
}

function PanelDetailsContent({
  details,
  onAddOriginFromNode,
  defaultMaxDepth,
  defaultMaxNodes,
  isAddingOrigin,
  label,
  onSaveLabel,
  isSavingLabel,
}: {
  details: TransactionNodeDetails
  onAddOriginFromNode?: (params: {
    maxDepth: number
    maxNodes: number
    mode: 'fetch' | 'draw'
  }) => Promise<void> | void
  defaultMaxDepth: number
  defaultMaxNodes: number
  isAddingOrigin: boolean
  label: AddressLabel | null
  onSaveLabel?: (payload: {
    category: EntityCategory | string
    labelName: string
    sanctionList: SanctionList | string | null
  }) => Promise<void> | void
  isSavingLabel: boolean
}) {
  const [activeTab, setActiveTab] = useState<'transactions' | 'analytics' | 'label'>('transactions')
  const [extendMaxDepth, setExtendMaxDepth] = useState(String(defaultMaxDepth))
  const [extendMaxNodes, setExtendMaxNodes] = useState(String(defaultMaxNodes))
  const [originMode, setOriginMode] = useState<'fetch' | 'draw'>('fetch')
  const [labelNameInput, setLabelNameInput] = useState('')
  const [labelCategoryInput, setLabelCategoryInput] = useState<EntityCategory>('exchange')
  const [sanctionListInput, setSanctionListInput] = useState<SanctionList>('ofac')

  useEffect(() => {
    setActiveTab('transactions')
  }, [details.address])

  useEffect(() => {
    setExtendMaxDepth(String(defaultMaxDepth))
    setExtendMaxNodes(String(defaultMaxNodes))
    setOriginMode('fetch')
    setLabelNameInput('')
    setLabelCategoryInput('exchange')
    setSanctionListInput('ofac')
  }, [details.address, defaultMaxDepth, defaultMaxNodes])

  const tokenVolumes = useMemo(() => buildTokenVolumes(details.incoming, details.outgoing), [details.incoming, details.outgoing])
  const timelinePoints = useMemo(() => buildTimelinePoints(details.incoming, details.outgoing), [details.incoming, details.outgoing])
  const primaryLabel = label ? formatAddressLabel(label) : null

  return (
    <div className='flex min-h-0 flex-1 flex-col'>
      <div className='space-y-2 border-b border-border p-4 pb-4'>
        <div className='flex items-start justify-between gap-3'>
          <div className='space-y-2'>
            <div className='flex items-center gap-2'>
              <h2 className='font-mono text-base font-semibold'>{primaryLabel ?? details.node.label}</h2>
              {details.node.group ? (
                <Badge variant='outline'>{groupLabels[details.node.group] ?? details.node.group}</Badge>
              ) : null}
            </div>
            <p className='break-all font-mono text-xs text-muted-foreground'>{details.address}</p>
          </div>
          <CopyButton value={details.address} label='address' />
        </div>

        {onAddOriginFromNode ? (
          <div className='grid grid-cols-[repeat(2,minmax(0,1fr))_auto] items-end gap-2 rounded-md border border-border/70 bg-card/40 p-2'>
            <div className='space-y-1'>
              <Label htmlFor='drawer-max-depth' className='text-[11px] text-muted-foreground'>
                max_depth
              </Label>
              <Input
                id='drawer-max-depth'
                inputMode='numeric'
                size={16}
                value={extendMaxDepth}
                className='h-8 text-xs'
                onChange={(event) => setExtendMaxDepth(event.target.value)}
              />
            </div>
            <div className='space-y-1'>
              <Label htmlFor='drawer-max-nodes' className='text-[11px] text-muted-foreground'>
                max_nodes
              </Label>
              <Input
                id='drawer-max-nodes'
                inputMode='numeric'
                value={extendMaxNodes}
                className='h-8 text-xs'
                onChange={(event) => setExtendMaxNodes(event.target.value)}
              />
            </div>
            <div className='flex flex-wrap items-center justify-end gap-1'>
              <Button
                type='button'
                size='sm'
                variant={originMode === 'fetch' ? 'secondary' : 'ghost'}
                className='h-8 px-3 text-xs'
                onClick={() => setOriginMode('fetch')}
              >
                Fetch
              </Button>
              <Button
                type='button'
                size='sm'
                variant={originMode === 'draw' ? 'secondary' : 'ghost'}
                className='h-8 px-3 text-xs'
                onClick={() => setOriginMode('draw')}
              >
                Draw
              </Button>
              <Button
                type='button'
                className='h-8 px-3 text-xs'
                disabled={isAddingOrigin}
                onClick={() =>
                  void onAddOriginFromNode({
                    maxDepth: toPositiveInt(extendMaxDepth, defaultMaxDepth),
                    maxNodes: toPositiveInt(extendMaxNodes, defaultMaxNodes),
                    mode: originMode,
                  })
                }
              >
                {isAddingOrigin ? <Loader2 className='size-3 animate-spin' /> : null}
                Add as origin
              </Button>
            </div>
          </div>
        ) : null}
      </div>

      <div className='flex min-h-0 flex-1 flex-col'>
        <div className='border-b border-border px-4 py-2'>
          <ToggleGroup
            type='single'
            variant='outline'
            size='sm'
            value={activeTab}
            onValueChange={(value) => {
              if (value === 'transactions' || value === 'analytics' || value === 'label') {
                setActiveTab(value)
              }
            }}
            // className='grid w-full grid-cols-3'
            className='grid w-full grid-cols-2'
          >
            <ToggleGroupItem value='transactions' aria-label='Transactions tab' className='w-full'>
              Transactions
            </ToggleGroupItem>
            {/* <ToggleGroupItem value='analytics' aria-label='Analytics tab' className='w-full'>
              Analytics
            </ToggleGroupItem> */}
            <ToggleGroupItem value='label' aria-label='Label tab' className='w-full'>
              Label
            </ToggleGroupItem>
          </ToggleGroup>
        </div>

        {activeTab === 'transactions' ? (
          <div className='min-h-0 flex-1'>
          <ScrollArea className='h-full'>
            <motion.div
              className='space-y-4 p-4'
              variants={motionContainer}
              initial='hidden'
              animate='show'
              key={`${details.address}-transactions`}
            >
              <motion.section className='grid grid-cols-2 gap-3 text-sm' variants={motionItem}>
                <Stat label='Incoming' value={details.incoming.length} />
                <Stat label='Outgoing' value={details.outgoing.length} />
                <Stat label='Weight' value={details.node.weight?.toFixed(1) ?? '—'} />
                <Stat label='Connections' value={details.incoming.length + details.outgoing.length} />
              </motion.section>

              <Separator />

              <motion.div variants={motionItem}>
                <Accordion type='multiple' defaultValue={['incoming', 'outgoing']} className='w-full'>
                  <AccordionItem value='incoming'>
                    <AccordionTrigger>
                      <span className='inline-flex items-center gap-1'>
                        Incoming
                        <span className='font-normal text-muted-foreground'>({details.incoming.length})</span>
                      </span>
                    </AccordionTrigger>
                    <AccordionContent>
                      <TransactionList edges={details.incoming} direction='in' />
                    </AccordionContent>
                  </AccordionItem>
                  <AccordionItem value='outgoing'>
                    <AccordionTrigger>
                      <span className='inline-flex items-center gap-1'>
                        Outgoing
                        <span className='font-normal text-muted-foreground'>({details.outgoing.length})</span>
                      </span>
                    </AccordionTrigger>
                    <AccordionContent>
                      <TransactionList edges={details.outgoing} direction='out' />
                    </AccordionContent>
                  </AccordionItem>
                </Accordion>
              </motion.div>
            </motion.div>
          </ScrollArea>
          </div>
        ) : null}

        {activeTab === 'analytics' ? (
          <div className='min-h-0 flex-1'>
          <ScrollArea className='h-full'>
            <div className='space-y-5 p-4'>
              <AnalyticsSection title='Flow timeline plot'>
                <FlowTimelinePlot points={timelinePoints} />
              </AnalyticsSection>

              <AnalyticsSection title='Token volume plot'>
                {tokenVolumes.length > 0 ? (
                  <TokenVolumePlot items={tokenVolumes} />
                ) : (
                  <p className='text-xs text-muted-foreground'>No numeric volumes available.</p>
                )}
              </AnalyticsSection>

              <AnalyticsSection title='Direction share (count)'>
                <DirectionSplit incoming={details.incoming.length} outgoing={details.outgoing.length} />
              </AnalyticsSection>
            </div>
          </ScrollArea>
          </div>
        ) : null}

        {activeTab === 'label' ? (
          <div className='min-h-0 flex-1'>
            <ScrollArea className='h-full'>
              <div className='space-y-4 p-4'>
                {onSaveLabel ? (
                  <div className='space-y-2 rounded-md border border-border/70 bg-card/40 p-3'>
                    <h3 className='text-xs font-medium uppercase tracking-wide text-muted-foreground'>Edit label</h3>
                    <Input
                      value={labelNameInput}
                      placeholder='Label name'
                      className='h-8 text-xs'
                      onChange={(event) => setLabelNameInput(event.target.value)}
                    />

                    <div className='grid grid-cols-2 gap-1'>
                      {ENTITY_CATEGORIES.map((category) => (
                        <Button
                          key={category}
                          type='button'
                          size='sm'
                          variant={labelCategoryInput === category ? 'secondary' : 'ghost'}
                          className='h-8 px-2 text-xs'
                          onClick={() => setLabelCategoryInput(category)}
                        >
                          {category}
                        </Button>
                      ))}
                    </div>

                    {labelCategoryInput === 'sanctioned' ? (
                      <div className='grid grid-cols-4 gap-1'>
                        {SANCTION_LISTS.map((list) => (
                          <Button
                            key={list}
                            type='button'
                            size='sm'
                            variant={sanctionListInput === list ? 'secondary' : 'ghost'}
                            className='h-8 px-2 text-xs'
                            onClick={() => setSanctionListInput(list)}
                          >
                            {list}
                          </Button>
                        ))}
                      </div>
                    ) : null}

                    <Button
                      type='button'
                      className='w-full px-3 text-xs'
                      disabled={isSavingLabel}

                      onClick={() => {
                        const trimmed = labelNameInput.trim()
                        if (!trimmed) {
                          toast.error('Label name is required')
                          return
                        }
                        void onSaveLabel({
                          category: labelCategoryInput,
                          labelName: trimmed,
                          sanctionList: labelCategoryInput === 'sanctioned' ? sanctionListInput : null,
                        })
                      }}
                    >
                      {isSavingLabel ? <Loader2 className='size-3 animate-spin' /> : null}
                      Save label
                    </Button>
                  </div>
                ) : null}
              </div>
            </ScrollArea>
          </div>
        ) : null}
      </div>
    </div>
  )
}

function AnalyticsSection({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <section className='space-y-2'>
      <h3 className='text-xs font-medium uppercase tracking-wide text-muted-foreground'>{title}</h3>
      {children}
    </section>
  )
}

type BarItem = { label: string; value: number }

type TimelinePoint = {
  block: number
  incoming: number
  outgoing: number
}

function FlowTimelinePlot({ points }: { points: TimelinePoint[] }) {
  const [hoveredIndex, setHoveredIndex] = useState<number | null>(null)
  const [showIncoming, setShowIncoming] = useState(true)
  const [showOutgoing, setShowOutgoing] = useState(true)
  const [showZoomControls, setShowZoomControls] = useState(false)
  const [rangeStart, setRangeStart] = useState(0)
  const [rangeEnd, setRangeEnd] = useState(points.length > 0 ? points.length - 1 : 0)
  const hasData = points.length > 0
  const maxIndex = Math.max(points.length - 1, 0)

  const safeRangeStart = Math.min(rangeStart, maxIndex)
  const safeRangeEnd = Math.min(Math.max(rangeEnd, safeRangeStart), maxIndex)
  const visiblePoints = points.slice(safeRangeStart, safeRangeEnd + 1)
  const chartData = visiblePoints.length > 0 ? visiblePoints : points

  const width = 320
  const height = 140
  const padding = 16
  const plotWidth = width - padding * 2
  const plotHeight = height - padding * 2
  const maxY = Math.max(...chartData.map((point) => Math.max(point.incoming, point.outgoing)), 1)
  const stepX = chartData.length > 1 ? plotWidth / (chartData.length - 1) : 0

  const incomingPoints = chartData
    .map((point, index) => {
      const x = padding + index * stepX
      const y = padding + plotHeight - (point.incoming / maxY) * plotHeight
      return `${x},${y}`
    })
    .join(' ')

  const outgoingPoints = chartData
    .map((point, index) => {
      const x = padding + index * stepX
      const y = padding + plotHeight - (point.outgoing / maxY) * plotHeight
      return `${x},${y}`
    })
    .join(' ')

  const chartPoints = chartData.map((point, index) => {
    const x = padding + index * stepX
    const incomingY = padding + plotHeight - (point.incoming / maxY) * plotHeight
    const outgoingY = padding + plotHeight - (point.outgoing / maxY) * plotHeight
    return { ...point, x, incomingY, outgoingY }
  })

  const fromBlock = chartData[0]?.block
  const toBlock = chartData[chartData.length - 1]?.block
  const focusedPoint = hoveredIndex == null ? null : chartPoints[hoveredIndex]

  useEffect(() => {
    setRangeStart(0)
    setRangeEnd(points.length > 0 ? points.length - 1 : 0)
    setHoveredIndex(null)
  }, [points])

  useEffect(() => {
    setHoveredIndex(null)
  }, [safeRangeStart, safeRangeEnd, showIncoming, showOutgoing])

  if (!hasData) {
    return <p className='text-xs text-muted-foreground'>No block data available.</p>
  }

  return (
    <div className='space-y-2 rounded-md border border-border/70 bg-card/40 p-2'>
      <div className='flex items-center justify-between gap-2 text-[11px]'>
        <div className='inline-flex items-center gap-1 rounded bg-muted/40 p-0.5'>
          <Button
            type='button'
            size='sm'
            variant={showIncoming ? 'secondary' : 'ghost'}
            className='h-6 px-2 text-[11px]'
            onClick={() => setShowIncoming((prev) => !prev)}
          >
            In
          </Button>
          <Button
            type='button'
            size='sm'
            variant={showOutgoing ? 'secondary' : 'ghost'}
            className='h-6 px-2 text-[11px]'
            onClick={() => setShowOutgoing((prev) => !prev)}
          >
            Out
          </Button>
        </div>
        {focusedPoint ? (
          <span className='font-mono text-muted-foreground'>
            Block {focusedPoint.block.toLocaleString()} • In {focusedPoint.incoming.toFixed(2)} • Out {focusedPoint.outgoing.toFixed(2)}
          </span>
        ) : (
          <span className='text-muted-foreground'>Hover points for exact values</span>
        )}
      </div>
      <div className='flex justify-end'>
        <Button
          type='button'
          variant='ghost'
          size='sm'
          className='h-6 px-2 text-[11px]'
          onClick={() => setShowZoomControls((prev) => !prev)}
        >
          {showZoomControls ? 'Hide zoom' : 'Show zoom'}
        </Button>
      </div>
      <svg viewBox={`0 0 ${width} ${height}`} className='h-36 w-full' onMouseLeave={() => setHoveredIndex(null)}>
        <line x1={padding} y1={padding + plotHeight} x2={padding + plotWidth} y2={padding + plotHeight} stroke='currentColor' opacity='0.2' />
        {showIncoming ? <polyline fill='none' stroke='rgb(16 185 129)' strokeWidth='2' points={incomingPoints} /> : null}
        {showOutgoing ? <polyline fill='none' stroke='rgb(245 158 11)' strokeWidth='2' points={outgoingPoints} /> : null}
        {focusedPoint ? (
          <line
            x1={focusedPoint.x}
            y1={padding}
            x2={focusedPoint.x}
            y2={padding + plotHeight}
            stroke='currentColor'
            opacity='0.2'
            strokeDasharray='3 3'
          />
        ) : null}
        {focusedPoint && showIncoming ? <circle cx={focusedPoint.x} cy={focusedPoint.incomingY} r='3.5' fill='rgb(16 185 129)' /> : null}
        {focusedPoint && showOutgoing ? <circle cx={focusedPoint.x} cy={focusedPoint.outgoingY} r='3.5' fill='rgb(245 158 11)' /> : null}
        {chartPoints.map((point, index) => {
          const start = point.x - (index === 0 ? 0 : stepX / 2)
          const end = point.x + (index === chartPoints.length - 1 ? 0 : stepX / 2)
          return (
            <rect
              key={`${point.block}-${index}`}
              x={start}
              y={padding}
              width={Math.max(end - start, 6)}
              height={plotHeight}
              fill='transparent'
              onMouseEnter={() => setHoveredIndex(index)}
              onFocus={() => setHoveredIndex(index)}
            />
          )
        })}
      </svg>
      {maxIndex > 0 && showZoomControls ? (
        <div className='space-y-1.5 rounded border border-border/60 bg-background/50 px-2 py-1.5'>
          <div className='flex items-center justify-between text-[11px] text-muted-foreground'>
            <span>Zoom range</span>
            <span className='font-mono'>
              {safeRangeStart + 1}-{safeRangeEnd + 1} / {points.length}
            </span>
          </div>
          <RangeTimelineSlider
            min={0}
            max={maxIndex}
            minSpan={1}
            value={{ from: safeRangeStart, to: safeRangeEnd }}
            onChange={({ from, to }) => {
              setRangeStart(from)
              setRangeEnd(to)
            }}
          />
          <div className='flex justify-end'>
            <Button
              type='button'
              variant='ghost'
              size='sm'
              className='h-6 px-2 text-[11px]'
              onClick={() => {
                setRangeStart(0)
                setRangeEnd(maxIndex)
              }}
            >
              Reset zoom
            </Button>
          </div>
        </div>
      ) : null}
      <div className='flex items-center justify-between text-[11px] text-muted-foreground'>
        <span>Block {fromBlock?.toLocaleString() ?? '—'}</span>
        <div className='flex items-center gap-3'>
          <span className='inline-flex items-center gap-1'>
            <span className='size-2 rounded-full bg-emerald-500/80' />
            In
          </span>
          <span className='inline-flex items-center gap-1'>
            <span className='size-2 rounded-full bg-amber-500/80' />
            Out
          </span>
        </div>
        <span>Block {toBlock?.toLocaleString() ?? '—'}</span>
      </div>
    </div>
  )
}

function TokenVolumePlot({ items }: { items: BarItem[] }) {
  const [activeLabel, setActiveLabel] = useState<string | null>(items[0]?.label ?? null)
  const width = 320
  const height = 150
  const padding = 14
  const barGap = 8
  const barWidth = (width - padding * 2 - barGap * Math.max(items.length - 1, 0)) / Math.max(items.length, 1)
  const max = Math.max(...items.map((item) => item.value), 1)
  const activeItem = items.find((item) => item.label === activeLabel) ?? items[0]

  return (
    <div className='space-y-2 rounded-md border border-border/70 bg-card/40 p-2'>
      <div className='text-[11px] text-muted-foreground'>
        {activeItem ? (
          <span className='font-mono'>
            {activeItem.label}: {activeItem.value.toFixed(2)}
          </span>
        ) : (
          <span>Hover bars to inspect values</span>
        )}
      </div>
      <svg viewBox={`0 0 ${width} ${height}`} className='h-36 w-full'>
        <line x1={padding} y1={height - padding} x2={width - padding} y2={height - padding} stroke='currentColor' opacity='0.2' />
        {items.map((item, index) => {
          const normalized = item.value / max
          const barHeight = normalized * (height - padding * 2)
          const x = padding + index * (barWidth + barGap)
          const y = height - padding - barHeight
          return (
            <rect
              key={item.label}
              x={x}
              y={y}
              width={Math.max(4, barWidth)}
              height={barHeight}
              rx={3}
              className={cn(
                'cursor-pointer transition-all',
                activeItem?.label === item.label ? 'fill-primary' : 'fill-primary/55 hover:fill-primary/80',
              )}
              onMouseEnter={() => setActiveLabel(item.label)}
              onFocus={() => setActiveLabel(item.label)}
              onClick={() => setActiveLabel(item.label)}
            />
          )
        })}
      </svg>
      <div className='grid grid-cols-2 gap-x-3 gap-y-1 text-[11px] text-muted-foreground'>
        {items.map((item) => (
          <div key={item.label} className='flex items-center justify-between gap-2'>
            <span>{item.label}</span>
            <span className='font-mono tabular-nums'>{item.value.toFixed(2)}</span>
          </div>
        ))}
      </div>
    </div>
  )
}

function DirectionSplit({ incoming, outgoing }: { incoming: number; outgoing: number }) {
  const total = incoming + outgoing
  const incomingPercent = total > 0 ? (incoming / total) * 100 : 0
  const outgoingPercent = total > 0 ? (outgoing / total) * 100 : 0

  return (
    <div className='space-y-2'>
      <div className='h-2 w-full overflow-hidden rounded bg-muted/50'>
        <div className='flex h-full w-full'>
          <div className='bg-emerald-500/70' style={{ width: `${incomingPercent}%` }} />
          <div className='bg-amber-500/70' style={{ width: `${outgoingPercent}%` }} />
        </div>
      </div>
      <div className='grid grid-cols-2 gap-2 text-xs'>
        <span className='text-muted-foreground'>Incoming: {incoming}</span>
        <span className='text-right text-muted-foreground'>Outgoing: {outgoing}</span>
      </div>
    </div>
  )
}

function CopyButton({ value, label }: { value: string; label: string }) {
  const [isCopying, setIsCopying] = useState(false)

  return (
    <Button
      type='button'
      variant='outline'
      size='sm'
      onClick={() => {
        void (async () => {
          setIsCopying(true)
          try {
            await copyToClipboard(value)
            toast.success(`${label} copied`)
          } catch {
            toast.error(`Failed to copy ${label}`)
          } finally {
            setIsCopying(false)
          }
        })()
      }}
    >
      {isCopying ? <Loader2 className='animate-spin' /> : <Copy />}
      Copy
    </Button>
  )
}

function Stat({ label, value }: { label: string; value: string | number }) {
  return (
    <div className='rounded-lg border border-border bg-card/50 px-3 py-2'>
      <p className='text-xs text-muted-foreground'>{label}</p>
      <p className='mt-0.5 font-medium tabular-nums'>{value}</p>
    </div>
  )
}

function TransactionList({ edges, direction }: { edges: TransactionEdge[]; direction: 'in' | 'out' }) {
  if (edges.length === 0) {
    return <p className='text-xs text-muted-foreground'>No transactions</p>
  }

  return (
    <ul className='space-y-2'>
      {edges.map((edge) => (
        <li key={`${edge.tx_hash}-${edge.index}-${direction}`} className='rounded-lg border border-border bg-card/40 p-3'>
          <div className='flex items-center justify-between gap-2'>
            <span className='text-sm font-medium tabular-nums'>
              {edge.formatted} {edge.symbol}
            </span>
            <Badge variant='secondary' className='font-mono text-[10px] uppercase'>
              {edge.kind}
            </Badge>
          </div>
          <div className='mt-2 flex items-center justify-between gap-2'>
            <p className='font-mono text-[11px] text-muted-foreground'>{shortHash(edge.tx_hash)}</p>
            <CopyButton value={edge.tx_hash} label='tx hash' />
          </div>
          <p className='mt-1 text-[11px] text-muted-foreground'>
            {direction === 'in' ? 'From' : 'To'}: {shortAddress(direction === 'in' ? edge.from : edge.to)}
          </p>
          <p className='text-[11px] text-muted-foreground'>Block {edge.block.toLocaleString()}</p>
        </li>
      ))}
    </ul>
  )
}

function shortHash(hash: string) {
  if (hash.length <= 16) {
    return hash
  }
  return `${hash.slice(0, 10)}…${hash.slice(-8)}`
}

function shortAddress(address: string) {
  if (address.length <= 12) {
    return address
  }
  return `${address.slice(0, 6)}…${address.slice(-4)}`
}

function formatAddressLabel(label: AddressLabel) {
  if (label.labelName) {
    return label.labelName
  }
  if (label.category === 'sanctioned' && label.sanctionList) {
    return `sanctioned:${label.sanctionList}`
  }
  return label.category
}

function parseFormattedValue(value: string) {
  const parsed = Number(value)
  if (!Number.isFinite(parsed)) {
    return 0
  }
  return parsed
}

function toPositiveInt(value: string, fallback: number) {
  const parsed = Number(value.trim())
  if (!Number.isInteger(parsed) || parsed <= 0) {
    return fallback
  }
  return parsed
}

function buildTokenVolumes(incoming: TransactionEdge[], outgoing: TransactionEdge[]) {
  const totals = new Map<string, number>()

  for (const edge of [...incoming, ...outgoing]) {
    const amount = parseFormattedValue(edge.formatted)
    if (amount <= 0) {
      continue
    }
    const symbol = edge.symbol || 'Unknown'
    totals.set(symbol, (totals.get(symbol) ?? 0) + amount)
  }

  return Array.from(totals.entries())
    .map(([label, value]) => ({ label, value }))
    .sort((a, b) => b.value - a.value)
    .slice(0, 6)
}

function buildTimelinePoints(incoming: TransactionEdge[], outgoing: TransactionEdge[]) {
  const byBlock = new Map<number, { incoming: number; outgoing: number }>()

  for (const edge of incoming) {
    const entry = byBlock.get(edge.block) ?? { incoming: 0, outgoing: 0 }
    entry.incoming += parseFormattedValue(edge.formatted)
    byBlock.set(edge.block, entry)
  }

  for (const edge of outgoing) {
    const entry = byBlock.get(edge.block) ?? { incoming: 0, outgoing: 0 }
    entry.outgoing += parseFormattedValue(edge.formatted)
    byBlock.set(edge.block, entry)
  }

  return Array.from(byBlock.entries())
    .sort((a, b) => a[0] - b[0])
    .slice(-24)
    .map(([block, volume]) => ({
      block,
      incoming: volume.incoming,
      outgoing: volume.outgoing,
    }))
}
