import type { TransactionEdge, TransactionGraphPage } from '@/entities/transaction/model/transaction'
import type { GraphData, GraphNode } from '@/shared/graph'

export type TransactionNodeDetails = {
  node: GraphNode
  address: string
  incoming: TransactionEdge[]
  outgoing: TransactionEdge[]
}

export function getTransactionNodeDetails(
  graphPage: TransactionGraphPage,
  graphData: GraphData,
  nodeId: string,
): TransactionNodeDetails | null {
  const node = graphData.nodes.find((item) => item.id === nodeId)
  if (!node) {
    return null
  }

  return {
    node,
    address: nodeId,
    incoming: graphPage.edges.filter((edge) => edge.to === nodeId),
    outgoing: graphPage.edges.filter((edge) => edge.from === nodeId),
  }
}
