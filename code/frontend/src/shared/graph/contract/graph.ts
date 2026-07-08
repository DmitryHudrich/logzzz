export type GraphNode = {
  id: string
  label: string
  group?: string
  weight?: number
}

export type GraphEdge = {
  id: string
  source: string
  target: string
  label?: string
  weight?: number
}

export type GraphData = {
  nodes: GraphNode[]
  edges: GraphEdge[]
}

export const GRAPH_LAYOUT_OPTIONS = [
  { value: 'force', label: 'Force', description: 'Organic force-directed layout' },
  { value: 'breadthfirst', label: 'Flow', description: 'Layers by graph distance' },
  { value: 'spiral', label: 'Spiral', description: 'Readable spacing for dense graphs' },
  { value: 'grid', label: 'Grid', description: 'Even grid for dense graphs' },
] as const

export type GraphLayoutMode = (typeof GRAPH_LAYOUT_OPTIONS)[number]['value']

export function isGraphLayoutMode(value: string): value is GraphLayoutMode {
  return GRAPH_LAYOUT_OPTIONS.some((option) => option.value === value)
}

export type GraphAdapterProps = {
  graph: GraphData
  layout: GraphLayoutMode
  rootNodeIds?: ReadonlySet<string> | null
  selectedNodeId?: string
  visibleNodeIds?: ReadonlySet<string> | null
  visibleEdgeIds?: ReadonlySet<string> | null
  onNodeSelect?: (nodeId: string) => void
  onNodeHover?: (nodeId: string | null) => void
}
