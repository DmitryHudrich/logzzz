import { lazy, Suspense, useMemo, useState } from 'react'
import { AnimatePresence } from 'framer-motion'
import { AlertCircle, Loader2 } from 'lucide-react'
import { toast } from 'sonner'

import {
  emptyTransactionGraphPage,
  filterGraphData,
  getTransactionNodeDetails,
  mockTransactionGraphPage,
  transactionGraphPageToGraphData,
  useFetchTransactionGraphMutation,
  useIngestJobStatusQuery,
  useIngestWalletMutation,
  waitForIngestJob,
  type FetchGraphPayload,
  type TransactionGraphPage as TransactionGraphPageData,
} from '@/entities/transaction'
import { TransactionGraphControls } from '@/features/transaction-graph-controls'
import { TransactionGraphFlowForm } from '@/features/transaction-graph-flow'
import type { GraphFlowFormValues } from '@/features/transaction-graph-flow/model/form-schema'
import { TransactionGraphSourceToggle, type GraphSourceMode } from '@/features/transaction-graph-source'
import { TransactionNodeDetailsDrawer } from '@/features/transaction-node-details'
import { getErrorMessage } from '@/shared/api'
import { type GraphLayoutMode } from '@/shared/graph'
import { useDebouncedValue } from '@/shared/lib/use-debounced-value'
import { Alert, AlertDescription, AlertTitle } from '@/shared/ui/alert'
import { FadeIn } from '@/shared/ui/motion'
import { ScrollArea } from '@/shared/ui/scroll-area'
import { Separator } from '@/shared/ui/separator'
import { Skeleton } from '@/shared/ui/skeleton'
import { Tabs, TabsList, TabsTrigger } from '@/shared/ui/tabs'
import { Tooltip, TooltipContent, TooltipTrigger } from '@/shared/ui/tooltip'
import { cn } from '@/shared/lib/cn'
import { TransactionGraphLegend } from '@/widgets/transaction-graph/ui/TransactionGraphLegend'

const TransactionGraphWidget = lazy(async () => {
  const module = await import('@/widgets/transaction-graph/ui/TransactionGraphWidget')
  return { default: module.TransactionGraphWidget }
})

const defaultFormValues: GraphFlowFormValues = {
  address: '',
  fromBlock: '',
  maxDepth: '3',
  maxNodes: '500',
}

export const TransactionGraphPage = () => {
  const [layout, setLayout] = useState<GraphLayoutMode>('force')
  const [selectedNodeId, setSelectedNodeId] = useState('')
  const [hoveredNodeId, setHoveredNodeId] = useState<string | null>(null)
  const [query, setQuery] = useState('')
  const [sourceMode, setSourceMode] = useState<GraphSourceMode>('mock')
  const [backendGraph, setBackendGraph] = useState<TransactionGraphPageData | null>(null)
  const [ingestJobId, setIngestJobId] = useState<string | null>(null)
  const [statusMessage, setStatusMessage] = useState<string | null>(null)
  const [isLoadingGraph, setIsLoadingGraph] = useState(false)
  const [mobileTab, setMobileTab] = useState('graph')

  const debouncedQuery = useDebouncedValue(query, 200)
  const ingestMutation = useIngestWalletMutation()
  const drawGraphMutation = useFetchTransactionGraphMutation()
  const jobStatusQuery = useIngestJobStatusQuery(ingestJobId)

  const graphPage =
    sourceMode === 'mock' ? mockTransactionGraphPage : (backendGraph ?? emptyTransactionGraphPage)
  const hasBackendData = backendGraph !== null
  const showBackendEmptyState = sourceMode === 'backend' && !hasBackendData && !isLoadingGraph

  const baseGraph = useMemo(() => transactionGraphPageToGraphData(graphPage), [graphPage])

  const filteredGraph = useMemo(
    () => filterGraphData(baseGraph, graphPage, debouncedQuery),
    [baseGraph, debouncedQuery, graphPage],
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

  const selectedNodeLabel = useMemo(() => {
    if (!selectedNodeId) {
      return null
    }
    return baseGraph.nodes.find((node) => node.id === selectedNodeId)?.label ?? selectedNodeId
  }, [baseGraph.nodes, selectedNodeId])

  const hoveredNodeLabel = useMemo(() => {
    if (!hoveredNodeId) {
      return null
    }
    return baseGraph.nodes.find((node) => node.id === hoveredNodeId)?.label ?? hoveredNodeId
  }, [baseGraph.nodes, hoveredNodeId])

  const selectedNodeDetails = useMemo(() => {
    if (!selectedNodeId) {
      return null
    }
    return getTransactionNodeDetails(graphPage, baseGraph, selectedNodeId)
  }, [baseGraph, graphPage, selectedNodeId])

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

  const onSourceModeChange = (mode: GraphSourceMode) => {
    setSourceMode(mode)
    setSelectedNodeId('')

    if (mode === 'mock') {
      setStatusMessage(null)
      return
    }

    if (!backendGraph) {
      setStatusMessage(null)
      return
    }

    setStatusMessage(`Graph loaded: ${backendGraph.total_nodes} nodes, ${backendGraph.total_edges} edges.`)
  }

  const applyLoadedGraph = (page: TransactionGraphPageData) => {
    setBackendGraph(page)
    setSourceMode('backend')
    setSelectedNodeId('')
    setStatusMessage(`Graph loaded: ${page.total_nodes} nodes, ${page.total_edges} edges.`)
    toast.success(`Graph loaded: ${page.total_nodes} nodes, ${page.total_edges} edges`)
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
    setIsLoadingGraph(true)
    setStatusMessage('Starting ingest job...')

    try {
      const result = await ingestMutation.mutateAsync(payload)
      setIngestJobId(result.job_id)
      setStatusMessage(`Ingest job accepted: ${result.job_id}. Waiting for completion...`)

      await waitForIngestJob(result.job_id)
      setStatusMessage('Loading graph from backend...')

      const page = await drawGraphMutation.mutateAsync(payload)
      applyLoadedGraph(page)
    } catch (error) {
      setStatusMessage(null)
      toast.error(getErrorMessage(error, 'Failed to load graph.'))
    } finally {
      setIsLoadingGraph(false)
    }
  }

  const onDrawGraphOnly = async (payload: FetchGraphPayload) => {
    try {
      const page = await drawGraphMutation.mutateAsync(payload)
      applyLoadedGraph(page)
    } catch (error) {
      setStatusMessage(null)
      toast.error(getErrorMessage(error, 'Failed to load graph.'))
    }
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

  const isBackendMode = sourceMode === 'backend'
  const showGraphLoadingOverlay = isBackendMode && isLoadingGraph

  const resolvedStatusMessage = isBackendMode ? (jobStatusMessage ?? statusMessage) : null
  const resolvedErrorMessage = isBackendMode
    ? drawGraphMutation.error || ingestMutation.error
      ? getErrorMessage(drawGraphMutation.error ?? ingestMutation.error, 'Request failed.')
      : null
    : null

  const sidebar = (
    <div className='flex h-full flex-col gap-4'>
      <div className='flex flex-col gap-1'>
        <h1 className='text-2xl font-semibold tracking-tight'>PayTraces</h1>
        <p className='text-xs text-muted-foreground'>Transaction graph explorer</p>
      </div>

      <TransactionGraphSourceToggle value={sourceMode} onChange={onSourceModeChange} />

      <Separator />

      <TransactionGraphFlowForm
        defaultValues={defaultFormValues}
        onLoadGraph={onLoadGraph}
        onFetchOnly={onFetchOnly}
        onDrawGraph={onDrawGraphOnly}
        isLoading={isBackendMode && isLoadingGraph}
        isFetchingOnly={isBackendMode && ingestMutation.isPending && !isLoadingGraph}
        isDrawingGraph={isBackendMode && drawGraphMutation.isPending}
        ingestJobId={isBackendMode ? ingestJobId : null}
        ingestProgress={isBackendMode ? ingestProgress : null}
        ingestStatus={isBackendMode ? ingestStatus : null}
        statusMessage={resolvedStatusMessage}
        errorMessage={resolvedErrorMessage}
      />

      <TransactionGraphControls
        query={query}
        onQueryChange={setQuery}
        layout={layout}
        onLayoutChange={setLayout}
        nodeCount={filteredGraph.nodes.length}
        edgeCount={filteredGraph.edges.length}
        selectedNodeLabel={selectedNodeLabel}
      />
    </div>
  )

  const graphSection = (
    <section className='relative flex min-h-0 flex-1 flex-col p-3 lg:h-full'>
      <TransactionGraphLegend />

      {hoveredNodeLabel && !selectedNodeId ? (
        <div className='pointer-events-none absolute top-3 right-3 z-10'>
          <Tooltip open>
            <TooltipTrigger asChild>
              <span className='sr-only'>{hoveredNodeLabel}</span>
            </TooltipTrigger>
            <TooltipContent side='left'>{hoveredNodeLabel}</TooltipContent>
          </Tooltip>
        </div>
      ) : null}

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
            graph={baseGraph}
            layout={layout}
            selectedNodeId={selectedNodeId}
            visibleNodeIds={visibleNodeIds}
            visibleEdgeIds={visibleEdgeIds}
            onSelectNode={setSelectedNodeId}
            onHoverNode={setHoveredNodeId}
          />
        </Suspense>
      </div>

      <AnimatePresence>
        {showBackendEmptyState ? (
          <FadeIn className='pointer-events-none absolute inset-3 flex items-center justify-center rounded-xl border border-dashed border-border bg-background/80 backdrop-blur-sm'>
            <div className='max-w-sm px-6 text-center'>
              <p className='text-sm font-medium'>No backend data loaded</p>
              <p className='mt-2 text-xs text-muted-foreground'>
                Use Load graph for ingest + fetch, or Fetch graph to render from data already in the backend.
              </p>
            </div>
          </FadeIn>
        ) : null}
      </AnimatePresence>

      <AnimatePresence>
        {showGraphLoadingOverlay ? (
          <FadeIn className='absolute inset-3 flex items-center justify-center rounded-xl bg-background/70 backdrop-blur-sm'>
            <div className='flex items-center gap-2 text-sm text-muted-foreground'>
              <Loader2 className='size-4 animate-spin' />
              Loading graph...
            </div>
          </FadeIn>
        ) : null}
      </AnimatePresence>

      <AnimatePresence>
        {resolvedErrorMessage && sourceMode === 'backend' && !isLoadingGraph ? (
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
          <Tabs value={mobileTab} onValueChange={setMobileTab} className='shrink-0 lg:hidden'>
            <TabsList className='mx-3 mt-3 grid w-auto grid-cols-2'>
              <TabsTrigger value='graph'>Graph</TabsTrigger>
              <TabsTrigger value='controls'>Controls</TabsTrigger>
            </TabsList>
          </Tabs>

          <div
            className={cn(
              'min-h-0 flex-1 flex-col',
              mobileTab === 'controls' ? 'hidden lg:flex' : 'flex',
            )}
          >
            {graphSection}
          </div>

          <div
            className={cn(
              'min-h-0 flex-1 overflow-hidden lg:hidden',
              mobileTab === 'graph' ? 'hidden' : 'flex flex-col',
            )}
          >
            <ScrollArea className='h-full'>
              <div className='p-4'>{sidebar}</div>
            </ScrollArea>
          </div>
        </div>

        <aside className='hidden h-full w-1/5 min-w-[320px] border-l border-border bg-background lg:flex lg:flex-col'>
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
      />
    </main>
  )
}
