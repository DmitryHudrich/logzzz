import type { TransactionGraphPage } from '@/entities/transaction/model/transaction'

export const emptyTransactionGraphPage: TransactionGraphPage = {
  total_nodes: 0,
  total_edges: 0,
  page: 0,
  page_size: 100,
  total_pages: 0,
  has_next: false,
  nodes: [],
  edges: [],
}
