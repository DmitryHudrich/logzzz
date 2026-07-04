import { zodResolver } from '@hookform/resolvers/zod'
import { ChevronDown, Download, Loader2, Network } from 'lucide-react'
import { useForm } from 'react-hook-form'

import {
  graphFlowFormSchema,
  graphFlowFormToPayload,
  type GraphFlowFormValues,
} from '@/features/transaction-graph-flow/model/form-schema'
import { Alert, AlertDescription, AlertTitle } from '@/shared/ui/alert'
import { Button } from '@/shared/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/shared/ui/card'
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from '@/shared/ui/collapsible'
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

  return (
    <Card className='gap-4 py-4'>
      <CardHeader className='px-4 pb-0'>
        <CardTitle className='text-base'>Wallet</CardTitle>
        <CardDescription>Ingest data, fetch the graph, or run both in one step.</CardDescription>
      </CardHeader>

      <CardContent className='space-y-4 px-4'>
        <div className='space-y-2'>
          <div className='flex items-center gap-2'>
            <Label htmlFor='address'>Wallet address</Label>
            <Tooltip>
              <TooltipTrigger asChild>
                <span className='cursor-help text-xs text-muted-foreground'>(required)</span>
              </TooltipTrigger>
              <TooltipContent>Root address used to build the transaction graph.</TooltipContent>
            </Tooltip>
          </div>
          <Input id='address' placeholder='0x...' {...form.register('address')} />
          {form.formState.errors.address ? (
            <p className='text-xs text-destructive'>{form.formState.errors.address.message}</p>
          ) : null}
        </div>

        <div className='space-y-2'>
          <div className='flex items-center gap-2'>
            <Label htmlFor='fromBlock'>from_block</Label>
            <Tooltip>
              <TooltipTrigger asChild>
                <span className='cursor-help text-xs text-muted-foreground'>(required)</span>
              </TooltipTrigger>
              <TooltipContent>Earliest block number to start scanning from.</TooltipContent>
            </Tooltip>
          </div>
          <Input id='fromBlock' inputMode='numeric' placeholder='19000000' {...form.register('fromBlock')} />
          {form.formState.errors.fromBlock ? (
            <p className='text-xs text-destructive'>{form.formState.errors.fromBlock.message}</p>
          ) : null}
        </div>

        <Collapsible>
          <CollapsibleTrigger asChild>
            <Button type='button' variant='ghost' size='sm' className='w-full justify-between px-0 hover:bg-transparent'>
              Advanced settings
              <ChevronDown className='size-4' />
            </Button>
          </CollapsibleTrigger>
          <CollapsibleContent className='space-y-4 pt-3'>
            <div className='space-y-2'>
              <Label htmlFor='maxDepth'>max_depth</Label>
              <Input id='maxDepth' inputMode='numeric' placeholder='3' {...form.register('maxDepth')} />
            </div>
            <div className='space-y-2'>
              <Label htmlFor='maxNodes'>max_nodes</Label>
              <Input id='maxNodes' inputMode='numeric' placeholder='500' {...form.register('maxNodes')} />
            </div>
          </CollapsibleContent>
        </Collapsible>

        <div className='flex flex-col gap-2'>
          <Button
            type='button'
            onClick={() => void submitPayload()}
            disabled={isLoading || isFetchingOnly || isDrawingGraph}
          >
            {isLoading ? <Loader2 className='animate-spin' /> : <Network />}
            {isLoading ? 'Loading graph...' : 'Load graph'}
          </Button>

          {onDrawGraph ? (
            <Button
              type='button'
              variant='secondary'
              onClick={() => void submitDrawGraph()}
              disabled={isLoading || isFetchingOnly || isDrawingGraph}
            >
              {isDrawingGraph ? <Loader2 className='animate-spin' /> : <Network />}
              {isDrawingGraph ? 'Fetching graph...' : 'Fetch graph'}
            </Button>
          ) : null}

          <Button
            type='button'
            variant='outline'
            onClick={() => void submitFetchOnly()}
            disabled={isLoading || isFetchingOnly || isDrawingGraph}
          >
            {isFetchingOnly ? <Loader2 className='animate-spin' /> : <Download />}
            {isFetchingOnly ? 'Fetching...' : 'Fetch data only'}
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
