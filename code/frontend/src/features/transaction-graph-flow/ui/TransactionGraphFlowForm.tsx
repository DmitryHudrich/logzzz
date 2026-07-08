import { zodResolver } from '@hookform/resolvers/zod'
import { Download, Loader2, Network } from 'lucide-react'
import { useEffect } from 'react'
import { useForm } from 'react-hook-form'

import {
  graphFlowFormSchema,
  graphFlowFormToPayload,
  type GraphFlowFormValues,
} from '@/features/transaction-graph-flow/model/form-schema'
import { Alert, AlertDescription, AlertTitle } from '@/shared/ui/alert'
import { Button } from '@/shared/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/shared/ui/card'
import { Input } from '@/shared/ui/input'
import { Label } from '@/shared/ui/label'
import { Progress } from '@/shared/ui/progress'
import { Tooltip, TooltipContent, TooltipTrigger } from '@/shared/ui/tooltip'

type TransactionGraphFlowFormProps = {
  defaultValues: GraphFlowFormValues
  onLoadGraph: (payload: ReturnType<typeof graphFlowFormToPayload>) => Promise<void>
  onFetchOnly: (payload: ReturnType<typeof graphFlowFormToPayload>) => Promise<void>
  onDrawGraph?: (payload: ReturnType<typeof graphFlowFormToPayload>) => Promise<void>
  isLoading: boolean
  isFetchingOnly: boolean
  isDrawingGraph?: boolean
  ingestJobId?: string | null
  ingestProgress?: number | null
  ingestStatus?: string | null
  statusMessage?: string | null
  errorMessage?: string | null
  onSettingsChange?: (payload: ReturnType<typeof graphFlowFormToPayload>) => void
  hideAddressInput?: boolean
}

export const TransactionGraphFlowForm = ({
  defaultValues,
  onLoadGraph,
  onFetchOnly,
  onDrawGraph,
  isLoading,
  isFetchingOnly,
  isDrawingGraph = false,
  ingestJobId,
  ingestProgress,
  ingestStatus,
  statusMessage,
  errorMessage,
  onSettingsChange,
  hideAddressInput = false,
}: TransactionGraphFlowFormProps) => {
  const form = useForm<GraphFlowFormValues>({
    resolver: zodResolver(graphFlowFormSchema),
    defaultValues,
    mode: 'onSubmit',
  })

  const submitPayload = form.handleSubmit(async (values) => {
    await onLoadGraph(graphFlowFormToPayload(values))
  })

  const submitFetchOnly = form.handleSubmit(async (values) => {
    await onFetchOnly(graphFlowFormToPayload(values))
  })

  const submitDrawGraph = form.handleSubmit(async (values) => {
    if (!onDrawGraph) {
      return
    }
    await onDrawGraph(graphFlowFormToPayload(values))
  })

  const showIngestProgress = ingestStatus === 'pending' || ingestStatus === 'running'

  useEffect(() => {
    if (!onSettingsChange) {
      return
    }

    onSettingsChange(graphFlowFormToPayload(form.getValues()))
    const subscription = form.watch((values) => {
      onSettingsChange(graphFlowFormToPayload(values as GraphFlowFormValues))
    })
    return () => {
      subscription.unsubscribe()
    }
  }, [form, onSettingsChange])

  return (
    <Card className='gap-4 py-4'>
      <CardHeader className='px-4 pb-0'>
        <CardTitle className='text-base'>Data ingestion</CardTitle>
        <CardDescription>Ingest data, fetch the graph, or run both in one step.</CardDescription>
      </CardHeader>

      <CardContent className='space-y-4 px-4'>
        {!hideAddressInput ? (
          <div className='space-y-2'>
            <div className='flex items-center gap-2'>
              <Label htmlFor='address'>First root wallet address</Label>
              <Tooltip>
                <TooltipTrigger asChild>
                  <span className='cursor-help text-xs text-muted-foreground'>(required)</span>
                </TooltipTrigger>
                <TooltipContent>Root address used to build first origin of the transaction graph.</TooltipContent>
              </Tooltip>
            </div>
            <Input id='address' placeholder='0x...' {...form.register('address')} />
            {form.formState.errors.address ? (
              <p className='text-xs text-destructive'>{form.formState.errors.address.message}</p>
            ) : null}
          </div>
        ) : null}

        <div className='grid grid-cols-2 gap-4'>
          <div className='space-y-2'>
            <div className='flex items-center gap-2'>
              <Label htmlFor='fromBlock'>from_block</Label>
              <Tooltip>
                <TooltipTrigger asChild>
                  <span className='cursor-help text-xs text-muted-foreground'>?</span>
                </TooltipTrigger>
                <TooltipContent>Earliest block number to start scanning from.</TooltipContent>
              </Tooltip>
            </div>
            <Input id='fromBlock' inputMode='numeric' placeholder='19000000' {...form.register('fromBlock')} />
            {form.formState.errors.fromBlock ? (
              <p className='text-xs text-destructive'>{form.formState.errors.fromBlock.message}</p>
            ) : null}
          </div>

          <div className='space-y-2'>
            <div className='flex items-center gap-2'>
              <Label htmlFor='toBlock'>to_block</Label>
              <Tooltip>
                <TooltipTrigger asChild>
                  <span className='cursor-help text-xs text-muted-foreground'>?</span>
                </TooltipTrigger>
                <TooltipContent>Latest block number to include in the scan.</TooltipContent>
              </Tooltip>
            </div>
            <Input id='toBlock' inputMode='numeric' placeholder='latest' {...form.register('toBlock')} />
            {form.formState.errors.toBlock ? (
              <p className='text-xs text-destructive'>{form.formState.errors.toBlock.message}</p>
            ) : null}
          </div>
        </div>

        <div className='grid grid-cols-1 gap-4 sm:grid-cols-2'>
          <div className='space-y-2'>
            <div className='flex items-center gap-2'>
              <Label htmlFor='maxDepth'>max_depth</Label>
              <Tooltip>
                <TooltipTrigger asChild>
                  <span className='cursor-help text-xs text-muted-foreground'>?</span>
                </TooltipTrigger>
                <TooltipContent>Maximum traversal depth for graph expansion.</TooltipContent>
              </Tooltip>
            </div>
            <Input id='maxDepth' inputMode='numeric' placeholder='2' {...form.register('maxDepth')} />
          </div>
          <div className='space-y-2'>
            <div className='flex items-center gap-2'>
              <Label htmlFor='maxNodes'>max_nodes</Label>
              <Tooltip>
                <TooltipTrigger asChild>
                  <span className='cursor-help text-xs text-muted-foreground'>?</span>
                </TooltipTrigger>
                <TooltipContent>Upper limit on nodes returned in the graph.</TooltipContent>
              </Tooltip>
            </div>
            <Input id='maxNodes' inputMode='numeric' placeholder='500' {...form.register('maxNodes')} />
          </div>
        </div>

        <div className='flex flex-col gap-2'>
          <Button
            type='button'
            onClick={() => void submitPayload()}
            disabled={isLoading || isFetchingOnly || isDrawingGraph}
          >
            {isLoading ? <Loader2 className='animate-spin' /> : <Network />}
            {isLoading ? 'Fetching graph...' : 'Fetch graph'}
          </Button>

          {onDrawGraph ? (
            <Button
              type='button'
              variant='secondary'
              onClick={() => void submitDrawGraph()}
              disabled={isLoading || isFetchingOnly || isDrawingGraph}
            >
              {isDrawingGraph ? <Loader2 className='animate-spin' /> : <Network />}
              {isDrawingGraph ? 'Drawing graph...' : 'Draw graph'}
            </Button>
          ) : null}

          <Button
            type='button'
            variant='outline'
            onClick={() => void submitFetchOnly()}
            disabled={isLoading || isFetchingOnly || isDrawingGraph}
          >
            {isFetchingOnly ? <Loader2 className='animate-spin' /> : <Download />}
            {isFetchingOnly ? 'Ingesting data...' : 'Ingest data'}
          </Button>
        </div>

        {showIngestProgress ? (
          <div className='space-y-2'>
            <div className='flex items-center justify-between text-xs text-muted-foreground'>
              <span>Ingest job {ingestStatus}</span>
              {ingestJobId ? <span className='font-mono'>{ingestJobId.slice(0, 8)}…</span> : null}
            </div>
            <Progress value={ingestProgress ?? 45} className={ingestProgress == null ? 'animate-pulse' : undefined} />
          </div>
        ) : null}

        {statusMessage ? (
          <Alert>
            <AlertTitle>Status</AlertTitle>
            <AlertDescription>{statusMessage}</AlertDescription>
          </Alert>
        ) : null}

        {errorMessage ? (
          <Alert variant='destructive'>
            <AlertTitle>Request failed</AlertTitle>
            <AlertDescription>{errorMessage}</AlertDescription>
          </Alert>
        ) : null}
      </CardContent>
    </Card>
  )
}
