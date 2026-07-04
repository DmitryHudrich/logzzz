import { motion } from 'framer-motion'
import { Copy, Loader2, X } from 'lucide-react'
import { useEffect, useState } from 'react'
import { toast } from 'sonner'

import type { TransactionEdge, TransactionNodeDetails } from '@/entities/transaction'
import { Accordion, AccordionContent, AccordionItem, AccordionTrigger } from '@/shared/ui/accordion'
import { Badge } from '@/shared/ui/badge'
import { Button } from '@/shared/ui/button'
import { copyToClipboard } from '@/shared/lib/copy-to-clipboard'
import { cn } from '@/shared/lib/cn'
import { motionContainer, motionItem } from '@/shared/ui/motion'
import { ScrollArea } from '@/shared/ui/scroll-area'
import { Separator } from '@/shared/ui/separator'
import { Skeleton } from '@/shared/ui/skeleton'

type TransactionNodeDetailsDrawerProps = {
  open: boolean
  onOpenChange: (open: boolean) => void
  details: TransactionNodeDetails | null
  isLoading?: boolean
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
          <PanelDetailsContent details={details} />
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

function PanelDetailsContent({ details }: { details: TransactionNodeDetails }) {
  return (
    <>
      <div className='space-y-2 border-b border-border p-4 pb-4'>
        <div className='flex items-start justify-between gap-3'>
          <div className='space-y-2'>
            <div className='flex items-center gap-2'>
              <h2 className='font-mono text-base font-semibold'>{details.node.label}</h2>
              {details.node.group ? (
                <Badge variant='outline'>{groupLabels[details.node.group] ?? details.node.group}</Badge>
              ) : null}
            </div>
            <p className='break-all font-mono text-xs text-muted-foreground'>{details.address}</p>
          </div>
          <CopyButton value={details.address} label='address' />
        </div>
      </div>

      <ScrollArea className='flex-1'>
        <motion.div
          className='space-y-4 p-4'
          variants={motionContainer}
          initial='hidden'
          animate='show'
          key={details.address}
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
    </>
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
