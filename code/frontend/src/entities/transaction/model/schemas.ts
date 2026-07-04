import { z } from 'zod'

export const transactionEdgeSchema = z.object({
  tx_hash: z.string(),
  index: z.number(),
  from: z.string(),
  to: z.string(),
  raw: z.string(),
  formatted: z.string(),
  symbol: z.string(),
  decimals: z.number(),
  block: z.number(),
  ts: z.number(),
  kind: z.enum(['native', 'token', 'internal', 'fee', 'utxo_edge']),
  chain_id: z.number(),
  contract: z.string().nullable().optional(),
})

export const transactionGraphPageSchema = z.object({
  total_nodes: z.number(),
  total_edges: z.number(),
  page: z.number(),
  page_size: z.number(),
  total_pages: z.number(),
  has_next: z.boolean(),
  nodes: z.array(z.string()),
  edges: z.array(transactionEdgeSchema),
})

export const ingestJobResponseSchema = z.object({
  job_id: z.string(),
})

export const jobStatusResponseSchema = z.object({
  id: z.string(),
  status: z.string(),
  error: z.string().nullable().optional(),
  created_at: z.string(),
  updated_at: z.string(),
})

export type TransactionEdgeDto = z.infer<typeof transactionEdgeSchema>
export type TransactionGraphPageDto = z.infer<typeof transactionGraphPageSchema>
export type IngestJobResponseDto = z.infer<typeof ingestJobResponseSchema>
export type JobStatusResponseDto = z.infer<typeof jobStatusResponseSchema>

export function parseTransactionGraphPage(data: unknown) {
  return transactionGraphPageSchema.parse(data)
}

export function parseIngestJobResponse(data: unknown) {
  return ingestJobResponseSchema.parse(data)
}

export function parseJobStatusResponse(data: unknown) {
  return jobStatusResponseSchema.parse(data)
}
