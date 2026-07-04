import type { TransactionGraphPage } from '@/entities/transaction/model/transaction'
import type { GraphData } from '@/shared/graph'

export function filterGraphData(
  baseGraph: GraphData,
  graphPage: TransactionGraphPage,
  query: string,
): GraphData {
  const normalized = query.trim().toLowerCase()
  if (!normalized) {
    return baseGraph
  }

  const visibleNodeIds = new Set(
    baseGraph.nodes
      .filter((node) => node.id.toLowerCase().includes(normalized) || node.label.toLowerCase().includes(normalized))
      .map((node) => node.id),
  )

  const matchingEdges = graphPage.edges.filter((edge) => {
    const edgeText = `${edge.formatted} ${edge.symbol} ${edge.tx_hash}`.toLowerCase()
    return edgeText.includes(normalized) || visibleNodeIds.has(edge.from) || visibleNodeIds.has(edge.to)
  })

  matchingEdges.forEach((edge) => {
    visibleNodeIds.add(edge.from)
    visibleNodeIds.add(edge.to)
  })

  const nodes = baseGraph.nodes.filter((node) => visibleNodeIds.has(node.id))
  const edgeIds = new Set(matchingEdges.map((edge, idx) => `${edge.tx_hash}-${edge.index}-${idx}`))
  const edges = baseGraph.edges.filter((edge) => edgeIds.has(edge.id))

  return { nodes, edges }
}
