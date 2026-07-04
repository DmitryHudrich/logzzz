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

export type GraphLayoutMode = 'force' | 'concentric' | 'breadthfirst'

export type GraphAdapterProps = {
  graph: GraphData
  layout: GraphLayoutMode
  selectedNodeId?: string
  visibleNodeIds?: ReadonlySet<string> | null
  visibleEdgeIds?: ReadonlySet<string> | null
  onNodeSelect?: (nodeId: string) => void
  onNodeHover?: (nodeId: string | null) => void
}
