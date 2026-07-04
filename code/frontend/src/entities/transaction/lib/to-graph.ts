import type { GraphData, GraphNode } from '@/shared/graph'
import type { TransactionEdge, TransactionGraphPage } from '@/entities/transaction/model/transaction'

const exchangeAddresses = new Set([
  '0x5555555555555555555555555555555555555555',
  '0x6666666666666666666666666666666666666666',
  '0x7777777777777777777777777777777777777777',
  '0x8888888888888888888888888888888888888888',
])

const riskAddresses = new Set([
  '0x9999999999999999999999999999999999999999',
  '0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
  '0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
])

export function transactionGraphPageToGraphData(page: TransactionGraphPage): GraphData {
  const weighted = new Map<string, number>()
  const allAddresses = new Set(page.nodes)

  page.edges.forEach((edge) => {
    allAddresses.add(edge.from)
    allAddresses.add(edge.to)
    weighted.set(edge.from, (weighted.get(edge.from) ?? 0) + weightFromEdge(edge))
    weighted.set(edge.to, (weighted.get(edge.to) ?? 0) + weightFromEdge(edge))
  })

  const nodes: GraphNode[] = Array.from(allAddresses).map((address) => ({
    id: address,
    label: shortAddress(address),
    group: groupForAddress(address),
    weight: Math.max(1, Math.min(30, weighted.get(address) ?? 1)),
  }))

  const edges = page.edges.map((edge, idx) => ({
    id: `${edge.tx_hash}-${edge.index}-${idx}`,
    source: edge.from,
    target: edge.to,
    label: `${edge.formatted} ${edge.symbol}`,
    weight: weightFromEdge(edge),
  }))

  return { nodes, edges }
}

function weightFromEdge(edge: TransactionEdge) {
  const value = Number(edge.formatted)
  if (!Number.isFinite(value)) {
    return 1
  }
  return Math.max(1, Math.min(10, Math.log10(value + 1) * 2.2))
}

function shortAddress(address: string) {
  if (address.length <= 12) {
    return address
  }
  return `${address.slice(0, 6)}…${address.slice(-4)}`
}

function groupForAddress(address: string): string {
  if (riskAddresses.has(address)) {
    return 'risk'
  }
  if (exchangeAddresses.has(address)) {
    return 'exchange'
  }
  return 'wallet'
}
