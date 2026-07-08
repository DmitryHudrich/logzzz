import { lazy, Suspense, useEffect, useMemo, useRef, useState } from 'react'
import { AnimatePresence } from 'framer-motion'
import { AlertCircle, Eye, EyeOff, Loader2, RefreshCw } from 'lucide-react'
import { toast } from 'sonner'

import {
  emptyTransactionGraphPage,
  filterGraphData,
  getTransactionNodeDetails,
  transactionGraphPageToGraphData,
  useFetchTransactionGraphMutation,
  useIngestJobStatusQuery,
  useIngestWalletMutation,
  waitForIngestJob,
  type FetchGraphPayload,
  type TransactionGraphPage as TransactionGraphPageData,
} from '@/entities/transaction'
import { fetchAddressLabel, upsertAddressLabel, type AddressLabel, type EntityCategory, type SanctionList } from '@/entities/label'
import { TransactionGraphControls } from '@/features/transaction-graph-controls'
import { TransactionGraphFlowForm } from '@/features/transaction-graph-flow'
import { graphFlowFormToPayload, type GraphFlowFormValues } from '@/features/transaction-graph-flow/model/form-schema'
import { TransactionNodeDetailsDrawer } from '@/features/transaction-node-details'
import { getErrorMessage } from '@/shared/api'
import { type GraphLayoutMode } from '@/shared/graph'
import { cn } from '@/shared/lib/cn'
import { useDebouncedValue } from '@/shared/lib/use-debounced-value'
import { Alert, AlertDescription, AlertTitle } from '@/shared/ui/alert'
import { Button } from '@/shared/ui/button'
import { FadeIn } from '@/shared/ui/motion'
import { Input } from '@/shared/ui/input'
import { Label } from '@/shared/ui/label'
import { ScrollArea } from '@/shared/ui/scroll-area'
import { Skeleton } from '@/shared/ui/skeleton'
import { TransactionGraphLegend } from '@/widgets/transaction-graph/ui/TransactionGraphLegend'

const TransactionGraphWidget = lazy(async () => {
  const module = await import('@/widgets/transaction-graph/ui/TransactionGraphWidget')
  return { default: module.TransactionGraphWidget }
})

const defaultFormValues: GraphFlowFormValues = {
  address: '',
  fromBlock: '',
  toBlock: '',
  maxDepth: '2',
  maxNodes: '500',
}

type GraphSource = {
  id: string
  rootAddress: string
  payload: FetchGraphPayload
  graphPage: TransactionGraphPageData
  enabled: boolean
  isLoading: boolean
}

type SourceSettingsDraft = {
  maxDepth: string
  maxNodes: string
}

export const TransactionGraphPage = () => {
  const [layout, setLayout] = useState<GraphLayoutMode>('force')
  const [selectedNodeId, setSelectedNodeId] = useState('')
  const [query, setQuery] = useState('')
  const [graphSources, setGraphSources] = useState<GraphSource[]>([])
  const [activeSourceId, setActiveSourceId] = useState<string | null>(null)
  const [mainFormPayload, setMainFormPayload] = useState<FetchGraphPayload>(() => graphFlowFormToPayload(defaultFormValues))
  const [sourceSettingsDraft, setSourceSettingsDraft] = useState<SourceSettingsDraft | null>(null)
  const [ingestJobId, setIngestJobId] = useState<string | null>(null)
  const [statusMessage, setStatusMessage] = useState<string | null>(null)
  const [isLoadingGraph, setIsLoadingGraph] = useState(false)
  const [loadingSourceId, setLoadingSourceId] = useState<string | null>(null)
  const [selectedBlockRange, setSelectedBlockRange] = useState<{ from: number; to: number } | null>(null)
  const [rebuildMode, setRebuildMode] = useState<'fetch' | 'draw'>('fetch')
  const [labelByAddress, setLabelByAddress] = useState<Record<string, AddressLabel>>({})
  const [isSavingLabel, setIsSavingLabel] = useState(false)
  const loadedLabelAddressesRef = useRef<Set<string>>(new Set())

  const debouncedQuery = useDebouncedValue(query, 200)
  const ingestMutation = useIngestWalletMutation()
  const drawGraphMutation = useFetchTransactionGraphMutation()
  const jobStatusQuery = useIngestJobStatusQuery(ingestJobId)

  const activeSource = useMemo(
    () => graphSources.find((source) => source.id === activeSourceId) ?? null,
    [activeSourceId, graphSources],
  )
  const enabledSources = useMemo(() => graphSources.filter((source) => source.enabled), [graphSources])
  const enabledSourceKey = useMemo(
    () => enabledSources.map((source) => source.id).sort().join('|'),
    [enabledSources],
  )
  const graphPage = useMemo(
    () => mergeTransactionGraphPages(enabledSources.map((source) => source.graphPage)),
    [enabledSources],
  )
  const blockRange = useMemo(() => getBlockRangeFromEdges(graphPage.edges), [graphPage.edges])

  useEffect(() => {
    if (!blockRange) {
      setSelectedBlockRange(null)
      return
    }

    setSelectedBlockRange({ from: blockRange.from, to: blockRange.to })
  }, [blockRange?.from, blockRange?.to, enabledSourceKey])

  const blockFilteredGraphPage = useMemo(
    () => filterGraphPageByBlockRange(graphPage, selectedBlockRange),
    [graphPage, selectedBlockRange],
  )
  const hasBackendData = blockFilteredGraphPage.nodes.length > 0
  const showBackendEmptyState = !hasBackendData && !isLoadingGraph

  const baseGraph = useMemo(() => transactionGraphPageToGraphData(blockFilteredGraphPage), [blockFilteredGraphPage])
  const graphWithLabelName = useMemo(
    () => ({
      ...baseGraph,
      nodes: baseGraph.nodes.map((node) => {
        const label = labelByAddress[normalizeAddress(node.id)]
        if (!label) {
          return node
        }
        return {
          ...node,
          label: formatAddressLabel(label),
        }
      }),
    }),
    [baseGraph, labelByAddress],
  )
  const rootNodeIds = useMemo(
    () => new Set(enabledSources.map((source) => source.rootAddress)),
    [enabledSources],
  )

  const filteredGraph = useMemo(
    () => filterGraphData(graphWithLabelName, blockFilteredGraphPage, debouncedQuery),
    [graphWithLabelName, blockFilteredGraphPage, debouncedQuery],
  )

  const visibleNodeIds = useMemo(() => {
    if (!debouncedQuery.trim()) {
      return null
    }
    return new Set(filteredGraph.nodes.map((node) => node.id))
  }, [debouncedQuery, filteredGraph.nodes])

  const visibleEdgeIds = useMemo(() => {
    if (!debouncedQuery.trim()) {
      return null
    }
    return new Set(filteredGraph.edges.map((edge) => edge.id))
  }, [debouncedQuery, filteredGraph.edges])

  const selectedNodeDisplayLabel = useMemo(() => {
    if (!selectedNodeId) {
      return null
    }
    return graphWithLabelName.nodes.find((node) => node.id === selectedNodeId)?.label ?? selectedNodeId
  }, [graphWithLabelName.nodes, selectedNodeId])

  const selectedNodeAddressLabel = useMemo(() => {
    if (!selectedNodeId) {
      return null
    }
    return labelByAddress[normalizeAddress(selectedNodeId)] ?? null
  }, [labelByAddress, selectedNodeId])

  const selectedNodeDetails = useMemo(() => {
    if (!selectedNodeId) {
      return null
    }
    return getTransactionNodeDetails(blockFilteredGraphPage, graphWithLabelName, selectedNodeId)
  }, [blockFilteredGraphPage, graphWithLabelName, selectedNodeId])

  const ingestStatus = jobStatusQuery.data?.status.toLowerCase() ?? null
  const ingestProgress = useMemo(() => {
    if (ingestStatus === 'pending') {
      return 25
    }
    if (ingestStatus === 'running') {
      return 65
    }
    if (ingestStatus === 'done') {
      return 100
    }
    return null
  }, [ingestStatus])

  useEffect(() => {
    if (!selectedNodeId || baseGraph.nodes.some((node) => node.id === selectedNodeId)) {
      return
    }
    setSelectedNodeId('')
  }, [baseGraph.nodes, selectedNodeId])

  useEffect(() => {
    if (!activeSource) {
      setSourceSettingsDraft(null)
      return
    }
    setSourceSettingsDraft(payloadToSourceDraft(activeSource.payload))
    setRebuildMode('fetch')
  }, [activeSource])

  const upsertGraphSource = ({
    page,
    payload,
    sourceId,
  }: {
    page: TransactionGraphPageData
    payload: FetchGraphPayload
    sourceId: string
  }) => {
    const normalizedAddress = normalizeAddress(payload.address)

    setGraphSources((previous) => {
      const sourceIndex = previous.findIndex((source) => source.id === sourceId)
      if (sourceIndex < 0) {
        return [
          ...previous,
          {
            id: sourceId,
            rootAddress: normalizedAddress,
            payload: { ...payload, address: normalizedAddress },
            graphPage: page,
            enabled: true,
            isLoading: false,
          },
        ]
      }

      return previous.map((source) =>
        source.id === sourceId
          ? {
              ...source,
              rootAddress: normalizedAddress,
              payload: { ...payload, address: normalizedAddress },
              graphPage: page,
              enabled: true,
              isLoading: false,
            }
          : source,
      )
    })

    setActiveSourceId(sourceId)
    setSelectedNodeId('')
    setStatusMessage(`Source loaded: ${page.total_nodes} nodes, ${page.total_edges} edges.`)
    toast.success(`Source loaded: ${page.total_nodes} nodes, ${page.total_edges} edges`)
  }

  const loadGraphSource = async ({
    payload,
    withIngest,
    sourceId,
  }: {
    payload: FetchGraphPayload
    withIngest: boolean
    sourceId?: string
  }) => {
    const normalizedPayload = { ...payload, address: normalizeAddress(payload.address) }
    const effectiveSourceId = sourceId ?? createSourceId(normalizedPayload.address)
    setIsLoadingGraph(true)
    setLoadingSourceId(effectiveSourceId)
    setActiveSourceId(effectiveSourceId)
    setGraphSources((previous) => {
      const existingIndex = previous.findIndex((source) => source.id === effectiveSourceId)
      if (existingIndex < 0) {
        return [
          ...previous,
          {
            id: effectiveSourceId,
            rootAddress: normalizeAddress(normalizedPayload.address),
            payload: normalizedPayload,
            graphPage: emptyTransactionGraphPage,
            enabled: true,
            isLoading: true,
          },
        ]
      }
      return previous.map((source) =>
        source.id === effectiveSourceId
          ? {
              ...source,
              payload: normalizedPayload,
              enabled: true,
              isLoading: true,
            }
          : source,
      )
    })
    setStatusMessage(withIngest ? 'Starting ingest job...' : 'Loading graph from backend...')

    try {
      if (withIngest) {
        const ingestResult = await ingestMutation.mutateAsync(normalizedPayload)
        setIngestJobId(ingestResult.job_id)
        setStatusMessage(`Ingest job accepted: ${ingestResult.job_id}. Waiting for completion...`)

        await waitForIngestJob(ingestResult.job_id)
        setStatusMessage('Loading graph from backend...')
      }

      const page = await drawGraphMutation.mutateAsync(normalizedPayload)
      upsertGraphSource({ page, payload: normalizedPayload, sourceId: effectiveSourceId })
      await hydrateLabelForGraph(page)
    } catch (error) {
      setGraphSources((previous) =>
        previous
          .map((source) => (source.id === effectiveSourceId ? { ...source, isLoading: false } : source))
          .filter((source) => !(source.id === effectiveSourceId && !sourceId && source.graphPage.nodes.length === 0)),
      )
      setStatusMessage(null)
      toast.error(getErrorMessage(error, 'Failed to load graph source.'))
    } finally {
      setIsLoadingGraph(false)
      setLoadingSourceId(null)
    }
  }

  const onFetchOnly = async (payload: FetchGraphPayload) => {
    try {
      const result = await ingestMutation.mutateAsync(payload)
      setIngestJobId(result.job_id)
      setStatusMessage(`Ingest job accepted: ${result.job_id}. Waiting for completion...`)
      toast.message('Ingest job started')
    } catch (error) {
      setStatusMessage(null)
      toast.error(getErrorMessage(error, 'Failed to start ingest.'))
    }
  }

  const onLoadGraph = async (payload: FetchGraphPayload) => {
    await loadGraphSource({ payload, withIngest: true })
  }

  const onDrawGraphOnly = async (payload: FetchGraphPayload) => {
    await loadGraphSource({ payload, withIngest: false })
  }

  const applyFetchedLabel = (label: AddressLabel) => {
    setLabelByAddress((previous) => {
      const next = { ...previous }
      const normalizedAddresses = label.addresses.map((address) => normalizeAddress(address))
      for (const address of normalizedAddresses) {
        next[address] = label
      }
      return next
    })
    for (const address of label.addresses) {
      loadedLabelAddressesRef.current.add(normalizeAddress(address))
    }
  }

  const hydrateLabelForGraph = async (page: TransactionGraphPageData) => {
    const addresses = collectGraphAddresses(page).filter(
      (address) => !loadedLabelAddressesRef.current.has(normalizeAddress(address)),
    )
    if (addresses.length === 0) {
      return
    }

    addresses.forEach((address) => {
      loadedLabelAddressesRef.current.add(normalizeAddress(address))
    })

    const results = await Promise.allSettled(addresses.map((address) => fetchAddressLabel(address, 1)))
    results.forEach((result) => {
      if (result.status !== 'fulfilled' || !result.value) {
        return
      }
      applyFetchedLabel(result.value)
    })
  }

  const onAddOriginFromSelectedNode = async ({
    maxDepth,
    maxNodes,
    mode,
  }: {
    maxDepth: number
    maxNodes: number
    mode: 'fetch' | 'draw'
  }) => {
    if (!selectedNodeId) {
      return
    }
    await loadGraphSource({
      payload: {
        ...mainFormPayload,
        address: selectedNodeId,
        max_depth: maxDepth,
        max_nodes: maxNodes,
      },
      withIngest: mode === 'fetch',
    })
  }

  const onSaveLabelForSelectedNode = async (payload: {
    category: EntityCategory | string
    labelName: string
    sanctionList: SanctionList | string | null
  }) => {
    if (!selectedNodeId) {
      return
    }
    setIsSavingLabel(true)
    try {
      const label = await upsertAddressLabel({
        address: selectedNodeId,
        category: payload.category,
        labelName: payload.labelName,
        sanctionList: payload.sanctionList,
        chainId: 1,
      })
      applyFetchedLabel(label)
      toast.success('Label saved')
    } catch (error) {
      toast.error(getErrorMessage(error, 'Failed to save label.'))
    } finally {
      setIsSavingLabel(false)
    }
  }

  const onApplyActiveSourceSettings = async () => {
    if (!activeSource || !sourceSettingsDraft) {
      return
    }
    await loadGraphSource({
      payload: {
        address: activeSource.rootAddress,
        from_block: mainFormPayload.from_block,
        to_block: mainFormPayload.to_block,
        max_depth: parseNumberWithDefault(sourceSettingsDraft.maxDepth, 2),
        max_nodes: parseNumberWithDefault(sourceSettingsDraft.maxNodes, 500),
      },
      withIngest: rebuildMode === 'fetch',
      sourceId: activeSource.id,
    })
  }

  const onToggleSourceVisibility = (sourceId: string) => {
    setGraphSources((previous) =>
      previous.map((source) => (source.id === sourceId ? { ...source, enabled: !source.enabled } : source)),
    )
  }

  const jobStatusMessage = useMemo(() => {
    if (!ingestJobId || !jobStatusQuery.data || isLoadingGraph) {
      return null
    }

    if (ingestStatus === 'done') {
      return `Ingest job ${ingestJobId} completed. Use Fetch graph to render from stored data.`
    }

    if (ingestStatus === 'failed') {
      return jobStatusQuery.data.error ?? `Ingest job ${ingestJobId} failed.`
    }

    return `Ingest job ${ingestJobId}: ${jobStatusQuery.data.status}`
  }, [ingestJobId, ingestStatus, isLoadingGraph, jobStatusQuery.data])

  const hasLoadedSources = useMemo(
    () => graphSources.some((source) => source.graphPage.nodes.length > 0),
    [graphSources],
  )
  const showGraphLoadingOverlay = isLoadingGraph && !hasLoadedSources

  const resolvedStatusMessage = jobStatusMessage ?? statusMessage
  const resolvedErrorMessage =
    drawGraphMutation.error || ingestMutation.error
      ? getErrorMessage(drawGraphMutation.error ?? ingestMutation.error, 'Request failed.')
      : null

  const sidebar = (
    <div className='flex h-full flex-col gap-4'>
      <div className='flex flex-col gap-1'>
        <h1 className='text-2xl font-semibold tracking-tight'>PayTraces</h1>
        <p className='text-xs text-muted-foreground'>Transaction graph explorer</p>
      </div>

      <TransactionGraphFlowForm
        defaultValues={defaultFormValues}
        onLoadGraph={onLoadGraph}
        onFetchOnly={onFetchOnly}
        onDrawGraph={onDrawGraphOnly}
        isLoading={isLoadingGraph}
        isFetchingOnly={ingestMutation.isPending && !isLoadingGraph}
        isDrawingGraph={drawGraphMutation.isPending}
        ingestJobId={ingestJobId}
        ingestProgress={ingestProgress}
        ingestStatus={ingestStatus}
        statusMessage={resolvedStatusMessage}
        errorMessage={resolvedErrorMessage}
        onSettingsChange={setMainFormPayload}
        hideAddressInput={graphSources.length > 0}
      />

      <TransactionGraphControls
        query={query}
        onQueryChange={setQuery}
        layout={layout}
        onLayoutChange={setLayout}
        nodeCount={filteredGraph.nodes.length}
        edgeCount={filteredGraph.edges.length}
        selectedNodeLabel={selectedNodeDisplayLabel}
        blockRange={blockRange}
        selectedBlockRange={selectedBlockRange}
        onBlockRangeChange={setSelectedBlockRange}
      />
    </div>
  )

  const graphSection = (
    <section className='relative flex min-h-0 flex-1 flex-col p-3 lg:h-full'>
      {graphSources.length > 0 ? (
        <div className='mb-3 rounded-xl border border-border bg-card/60 p-2'>
          <div className='flex flex-wrap items-center justify-between gap-2'>
            <div className='flex min-w-0 flex-1 items-center gap-1 overflow-x-auto'>
              {graphSources.map((source) => {
                const isActive = source.id === activeSourceId
                const isSourceLoading = source.isLoading || source.id === loadingSourceId
                const normalizedRootAddress = normalizeAddress(source.rootAddress)
                const sourceLabel = labelByAddress[normalizedRootAddress]
                  ? formatAddressLabel(labelByAddress[normalizedRootAddress])
                  : shortAddress(source.rootAddress)
                return (
                  <div key={source.id} className='inline-flex items-center gap-1 rounded-md border border-border/60 bg-background/70 p-1'>
                    <button
                      type='button'
                      onClick={() => setActiveSourceId(source.id)}
                      title={sourceLabel}
                      className={cn(
                        'inline-flex items-center gap-1 rounded px-2 py-1 text-xs leading-none whitespace-nowrap transition-colors',
                        isActive ? 'bg-primary text-primary-foreground' : 'text-muted-foreground hover:bg-accent',
                      )}
                    >
                      {isSourceLoading ? <Loader2 className='size-3 shrink-0 animate-spin' /> : null}
                      <span className='max-w-44 truncate'>{sourceLabel}</span>
                    </button>
                    <Button
                      type='button'
                      size='sm'
                      variant='ghost'
                      className='h-6 px-1.5'
                      onClick={() => onToggleSourceVisibility(source.id)}
                      disabled={isSourceLoading}
                    >
                      {source.enabled ? <Eye className='size-3.5' /> : <EyeOff className='size-3.5' />}
                    </Button>
                  </div>
                )
              })}
            </div>

            <div />
          </div>

          {activeSource && sourceSettingsDraft ? (
            <div className='mt-2 grid gap-2 border-t border-border/70 pt-2 sm:grid-cols-[repeat(2,minmax(0,1fr))_auto]'>
              <div className='space-y-1'>
                <Label htmlFor='source-max-depth' className='text-[11px] text-muted-foreground'>
                  max_depth
                </Label>
                <Input
                  id='source-max-depth'
                  value={sourceSettingsDraft.maxDepth}
                  inputMode='numeric'
                  className='h-8 text-xs'
                  onChange={(event) =>
                    setSourceSettingsDraft((previous) =>
                      previous ? { ...previous, maxDepth: event.target.value } : previous,
                    )
                  }
                />
              </div>
              <div className='space-y-1'>
                <Label htmlFor='source-max-nodes' className='text-[11px] text-muted-foreground'>
                  max_nodes
                </Label>
                <Input
                  id='source-max-nodes'
                  value={sourceSettingsDraft.maxNodes}
                  inputMode='numeric'
                  className='h-8 text-xs'
                  onChange={(event) =>
                    setSourceSettingsDraft((previous) =>
                      previous ? { ...previous, maxNodes: event.target.value } : previous,
                    )
                  }
                />
              </div>

              <div className='flex flex-wrap items-end justify-end gap-1'>
                <Button
                  type='button'
                  size='sm'
                  variant={rebuildMode === 'fetch' ? 'secondary' : 'ghost'}
                  className='h-8 px-3 text-xs'
                  onClick={() => setRebuildMode('fetch')}
                >
                  Fetch
                </Button>
                <Button
                  type='button'
                  size='sm'
                  variant={rebuildMode === 'draw' ? 'secondary' : 'ghost'}
                  className='h-8 px-3 text-xs'
                  onClick={() => setRebuildMode('draw')}
                >
                  Draw
                </Button>
                <Button
                  type='button'
                  size='sm'
                  className='h-8 px-3 text-xs'
                  onClick={() => void onApplyActiveSourceSettings()}
                  disabled={isLoadingGraph || activeSource.isLoading}
                >
                  {activeSource.isLoading ? (
                    <Loader2 className='size-3 animate-spin' />
                  ) : (
                    <RefreshCw className='size-3' />
                  )}
                  Rebuild
                </Button>
              </div>
            </div>
          ) : null}
        </div>
      ) : null}

      <TransactionGraphLegend />

      <div className='relative min-h-0 flex-1'>
        <Suspense
          fallback={
            <div className='flex h-full min-h-90 flex-col gap-3 rounded-xl border border-border bg-card/40 p-4'>
              <Skeleton className='h-4 w-32' />
              <Skeleton className='h-full min-h-80 w-full' />
            </div>
          }
        >
          <TransactionGraphWidget
            graph={graphWithLabelName}
            layout={layout}
            rootNodeIds={rootNodeIds}
            selectedNodeId={selectedNodeId}
            visibleNodeIds={visibleNodeIds}
            visibleEdgeIds={visibleEdgeIds}
            onSelectNode={setSelectedNodeId}
          />
        </Suspense>

        <AnimatePresence>
          {showBackendEmptyState ? (
            <FadeIn className='pointer-events-none absolute inset-0 flex items-center justify-center rounded-xl border border-dashed border-border bg-background/80 backdrop-blur-sm'>
              <div className='max-w-sm px-6 text-center'>
                <p className='text-sm font-medium'>No backend data loaded</p>
                <p className='mt-2 text-xs text-muted-foreground'>
                  Add a source with Load graph or Fetch graph, then extend from selected nodes to combine multiple
                  sources.
                </p>
              </div>
            </FadeIn>
          ) : null}
        </AnimatePresence>

        <AnimatePresence>
          {showGraphLoadingOverlay ? (
            <FadeIn className='pointer-events-none absolute inset-0 flex items-center justify-center rounded-xl bg-background/65 backdrop-blur-sm'>
              <div className='flex items-center gap-2 text-sm text-muted-foreground'>
                <Loader2 className='size-4 animate-spin' />
                Loading graph...
              </div>
            </FadeIn>
          ) : null}
        </AnimatePresence>
      </div>

      <AnimatePresence>
        {resolvedErrorMessage && !isLoadingGraph ? (
          <FadeIn className='absolute inset-x-3 bottom-3'>
            <Alert variant='destructive'>
              <AlertCircle />
              <AlertTitle>Request failed</AlertTitle>
              <AlertDescription>{resolvedErrorMessage}</AlertDescription>
            </Alert>
          </FadeIn>
        ) : null}
      </AnimatePresence>
    </section>
  )

  return (
    <main className='flex h-screen w-full flex-col overflow-hidden bg-background text-foreground'>
      <div className='flex min-h-0 flex-1 flex-col lg:flex-row'>
        <div className='flex min-h-0 flex-1 flex-col lg:w-4/5'>
          <div className='min-h-0 flex-1 flex flex-col'>
            {graphSection}
          </div>

          <div className='min-h-0 max-h-[40vh] overflow-hidden border-t border-border lg:hidden'>
            <ScrollArea className='h-full'>
              <div className='p-4'>{sidebar}</div>
            </ScrollArea>
          </div>
        </div>

        <aside className='hidden h-full w-full max-w-md shrink-0 border-l border-border bg-background lg:flex lg:flex-col'>
          <ScrollArea className='h-full'>
            <div className='p-4'>{sidebar}</div>
          </ScrollArea>
        </aside>
      </div>

      <TransactionNodeDetailsDrawer
        open={Boolean(selectedNodeId)}
        onOpenChange={(open) => {
          if (!open) {
            setSelectedNodeId('')
          }
        }}
        details={selectedNodeDetails}
        isLoading={Boolean(selectedNodeId) && !selectedNodeDetails}
        onAddOriginFromNode={onAddOriginFromSelectedNode}
        defaultMaxDepth={mainFormPayload.max_depth ?? 2}
        defaultMaxNodes={mainFormPayload.max_nodes ?? 500}
        isAddingOrigin={isLoadingGraph}
        label={selectedNodeAddressLabel}
        onSaveLabel={onSaveLabelForSelectedNode}
        isSavingLabel={isSavingLabel}
      />
    </main>
  )
}

function normalizeAddress(address: string) {
  return address.trim().toLowerCase()
}

function formatAddressLabel(label: AddressLabel) {
  const name = label.labelName?.trim() ?? ''
  if (name) {
    return name
  }
  if (label.category === 'sanctioned') {
    return `sanctioned:${label.sanctionList ?? 'unknown'}`
  }
  return label.category
}

function collectGraphAddresses(page: TransactionGraphPageData) {
  const addresses = new Set<string>()
  page.nodes.forEach((address) => addresses.add(address))
  page.edges.forEach((edge) => {
    addresses.add(edge.from)
    addresses.add(edge.to)
  })
  return Array.from(addresses)
}

function shortAddress(address: string) {
  if (address.length <= 14) {
    return address
  }
  return `${address.slice(0, 8)}…${address.slice(-4)}`
}

function createSourceId(rootAddress: string) {
  return `${rootAddress}-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 7)}`
}

function parseNumberWithDefault(value: string, fallback: number) {
  const parsed = Number(value.trim())
  if (!Number.isInteger(parsed) || parsed < 0) {
    return fallback
  }
  return parsed
}

function payloadToSourceDraft(payload: FetchGraphPayload): SourceSettingsDraft {
  return {
    maxDepth: payload.max_depth == null ? '2' : String(payload.max_depth),
    maxNodes: payload.max_nodes == null ? '500' : String(payload.max_nodes),
  }
}

function mergeTransactionGraphPages(pages: TransactionGraphPageData[]) {
  if (pages.length === 0) {
    return emptyTransactionGraphPage
  }

  const nodeSet = new Set<string>()
  const edgeMap = new Map<string, TransactionGraphPageData['edges'][number]>()

  for (const page of pages) {
    for (const node of page.nodes) {
      nodeSet.add(node)
    }
    for (const edge of page.edges) {
      const edgeId = `${edge.tx_hash}:${edge.index}:${edge.from.toLowerCase()}:${edge.to.toLowerCase()}`
      if (!edgeMap.has(edgeId)) {
        edgeMap.set(edgeId, edge)
      }
    }
  }

  const nodes = Array.from(nodeSet)
  const edges = Array.from(edgeMap.values())

  return {
    total_nodes: nodes.length,
    total_edges: edges.length,
    page: 0,
    page_size: nodes.length,
    total_pages: 1,
    has_next: false,
    nodes,
    edges,
  }
}

function getBlockRangeFromEdges(edges: TransactionGraphPageData['edges']) {
  if (edges.length === 0) {
    return null
  }
  let from = Number.POSITIVE_INFINITY
  let to = Number.NEGATIVE_INFINITY
  for (const edge of edges) {
    if (edge.block < from) {
      from = edge.block
    }
    if (edge.block > to) {
      to = edge.block
    }
  }
  if (!Number.isFinite(from) || !Number.isFinite(to)) {
    return null
  }
  return { from, to }
}

function filterGraphPageByBlockRange(
  page: TransactionGraphPageData,
  range: { from: number; to: number } | null,
): TransactionGraphPageData {
  if (!range) {
    return page
  }

  const edges = page.edges.filter((edge) => edge.block >= range.from && edge.block <= range.to)
  const nodesSet = new Set<string>()
  for (const edge of edges) {
    nodesSet.add(edge.from)
    nodesSet.add(edge.to)
  }

  const nodes = Array.from(nodesSet)

  return {
    ...page,
    nodes,
    edges,
    total_nodes: nodes.length,
    total_edges: edges.length,
    page: 0,
    total_pages: 1,
    has_next: false,
    page_size: nodes.length,
  }
}
