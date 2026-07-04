export type TransactionEdge = {
  tx_hash: string
  index: number
  from: string
  to: string
  raw: string
  formatted: string
  symbol: string
  decimals: number
  block: number
  ts: number
  kind: 'native' | 'token' | 'internal' | 'fee' | 'utxo_edge'
  chain_id: number
  contract?: string | null
}

export type TransactionGraphPage = {
  total_nodes: number
  total_edges: number
  page: number
  page_size: number
  total_pages: number
  has_next: boolean
  nodes: string[]
  edges: TransactionEdge[]
}
