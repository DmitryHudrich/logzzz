import Graph from 'graphology'
import Sigma from 'sigma'
import { useEffect, useMemo, useRef, useState } from 'react'

import type { GraphAdapterProps, GraphData, GraphLayoutMode } from '@/shared/graph/contract/graph'
import { applyGraphLayout } from '@/shared/graph/model/apply-graph-layout'

const GRAPH_THEME = {
  canvas: '#09090b',
  edge: '#2a2a30',
  edgeActive: '#52525c',
  label: '#52525c',
  labelActive: '#a1a1aa',
  dimmedNode: '#27272a',
  rootNode: '#f59e0b',
  groups: {
    wallet: '#7eb6ff',
    exchange: '#c4b0f5',
    risk: '#f09494',
    default: '#a1a1aa',
  },
} as const

type SigmaNodeAttrs = {
  x: number
  y: number
  size: number
  label: string
  color: string
  group: string
}

type SigmaEdgeAttrs = {
  size: number
  color: string
}

type NodePosition = { x: number; y: number }

export const SigmaGraphAdapter = ({
  graph,
  layout,
  rootNodeIds = null,
  selectedNodeId = '',
  visibleNodeIds = null,
  visibleEdgeIds = null,
  onNodeSelect,
  onNodeHover,
}: GraphAdapterProps) => {
  const containerRef = useRef<HTMLDivElement | null>(null)
  const rendererRef = useRef<Sigma<SigmaNodeAttrs, SigmaEdgeAttrs> | null>(null)
  const graphRef = useRef<Graph<SigmaNodeAttrs, SigmaEdgeAttrs> | null>(null)
  const nodePositionsRef = useRef<Map<string, NodePosition>>(new Map())
  const rootNodeIdsRef = useRef<ReadonlySet<string> | null>(rootNodeIds)
  const selectedNodeIdRef = useRef(selectedNodeId)
  const visibleNodeIdsRef = useRef(visibleNodeIds)
  const visibleEdgeIdsRef = useRef(visibleEdgeIds)
  const onNodeSelectRef = useRef(onNodeSelect)
  const onNodeHoverRef = useRef(onNodeHover)
  const layoutRef = useRef(layout)
  const graphSignatureRef = useRef('')
  const [canRender, setCanRender] = useState(false)

  rootNodeIdsRef.current = rootNodeIds
  selectedNodeIdRef.current = selectedNodeId
  visibleNodeIdsRef.current = visibleNodeIds
  visibleEdgeIdsRef.current = visibleEdgeIds
  onNodeSelectRef.current = onNodeSelect
  onNodeHoverRef.current = onNodeHover

  const graphSignature = useMemo(() => graph.nodes.map((node) => node.id).join('|'), [graph.nodes])

  useEffect(() => {
    if (layoutRef.current !== layout) {
      nodePositionsRef.current.clear()
      layoutRef.current = layout
    }
  }, [layout])

  useEffect(() => {
    if (graphSignatureRef.current !== graphSignature) {
      nodePositionsRef.current.clear()
      graphSignatureRef.current = graphSignature
    }
  }, [graphSignature])

  const preparedGraph = useMemo(
    () => buildGraph(graph, layout, nodePositionsRef.current, rootNodeIds),
    [graph, layout, rootNodeIds],
  )

  useEffect(() => {
    const container = containerRef.current
    if (!container) {
      return
    }

    const updateContainerState = () => {
      const ready = container.clientWidth > 0 && container.clientHeight > 0
      if (!ready) {
        return
      }

      setCanRender(true)

      if (rendererRef.current) {
        rendererRef.current.resize()
      }
    }

    updateContainerState()

    const observer = new ResizeObserver(updateContainerState)
    observer.observe(container)

    return () => {
      observer.disconnect()
    }
  }, [])

  useEffect(() => {
    const container = containerRef.current
    if (!container || !canRender) {
      return
    }

    const sigmaGraph = preparedGraph

    const renderer = new Sigma<SigmaNodeAttrs, SigmaEdgeAttrs>(sigmaGraph, container, {
      renderEdgeLabels: false,
      defaultNodeType: 'circle',
      defaultEdgeType: 'arrow',
      labelColor: { color: GRAPH_THEME.label },
      edgeLabelColor: { color: GRAPH_THEME.label },
      labelDensity: 0.04,
      labelGridCellSize: 120,
      labelSize: 10,
      labelFont: 'ui-monospace, SFMono-Regular, Menlo, monospace',
      minCameraRatio: 0.2,
      maxCameraRatio: 5,
      stagePadding: 32,
      zoomToSizeRatioFunction: (ratio) => ratio,
      nodeReducer: (node, data) => {
        const visibleIds = visibleNodeIdsRef.current
        if (visibleIds && !visibleIds.has(node)) {
          return { ...data, hidden: true, label: '' }
        }

        const isRoot = isRootAddress(node, rootNodeIdsRef.current)
        const activeNodeId = selectedNodeIdRef.current
        if (!activeNodeId) {
          if (isRoot) {
            return {
              ...data,
              color: GRAPH_THEME.rootNode,
              size: data.size * 1.42,
              label: data.label,
              labelColor: GRAPH_THEME.labelActive,
              zIndex: 2,
            }
          }
          return {
            ...data,
            labelColor: GRAPH_THEME.label,
          }
        }

        if (node === activeNodeId) {
          return {
            ...data,
            color: isRoot ? GRAPH_THEME.rootNode : data.color,
            size: isRoot ? data.size * 1.55 : data.size * 1.15,
            label: data.label,
            labelColor: GRAPH_THEME.labelActive,
            zIndex: isRoot ? 2 : 1,
          }
        }

        const neighborSet = getNeighborhood(sigmaGraph, activeNodeId)
        if (neighborSet.has(node)) {
          if (isRoot) {
            return {
              ...data,
              color: GRAPH_THEME.rootNode,
              size: data.size * 1.12,
            }
          }
          return data
        }

        return {
          ...data,
          color: GRAPH_THEME.dimmedNode,
          label: '',
        }
      },
      edgeReducer: (edge, data) => {
        const visibleIds = visibleEdgeIdsRef.current
        if (visibleIds && !visibleIds.has(edge)) {
          return { ...data, hidden: true }
        }

        const activeNodeId = selectedNodeIdRef.current
        if (!activeNodeId) {
          return data
        }

        const [source, target] = sigmaGraph.extremities(edge)
        const isConnected = source === activeNodeId || target === activeNodeId

        return {
          ...data,
          color: isConnected ? GRAPH_THEME.edgeActive : data.color,
          size: isConnected ? Math.max(data.size * 1.15, data.size) : data.size,
        }
      },
    })

    rendererRef.current = renderer
    graphRef.current = sigmaGraph

    const dragState = bindNodeDragging(renderer, sigmaGraph, nodePositionsRef, (nodeId) => {
      onNodeSelectRef.current?.(nodeId)
    })

    const onEnterNode = ({ node }: { node: string }) => {
      onNodeHoverRef.current?.(node)
    }

    const onLeaveNode = () => {
      onNodeHoverRef.current?.(null)
    }

    renderer.on('enterNode', onEnterNode)
    renderer.on('leaveNode', onLeaveNode)

    return () => {
      renderer.off('enterNode', onEnterNode)
      renderer.off('leaveNode', onLeaveNode)
      dragState.cleanup()
      renderer.kill()
      rendererRef.current = null
      graphRef.current = null
    }
  }, [canRender, preparedGraph])

  useEffect(() => {
    rendererRef.current?.refresh()
  }, [selectedNodeId, visibleNodeIds, visibleEdgeIds, rootNodeIds])

  return (
    <div className='relative h-full min-h-[360px] w-full'>
      <div
        ref={containerRef}
        className='h-full w-full rounded-xl border border-zinc-800/60'
        style={{ backgroundColor: GRAPH_THEME.canvas }}
      />
      {!canRender ? (
        <div className='pointer-events-none absolute inset-0 flex items-center justify-center rounded-xl bg-background/40 text-xs text-muted-foreground'>
          Preparing graph...
        </div>
      ) : null}
    </div>
  )
}

function buildGraph(
  graphData: GraphData,
  layout: GraphLayoutMode,
  savedPositions: Map<string, NodePosition>,
  rootNodeIds: ReadonlySet<string> | null,
) {
  const graph = new Graph<SigmaNodeAttrs, SigmaEdgeAttrs>({ type: 'directed', multi: true, allowSelfLoops: false })

  graphData.nodes.forEach((node) => {
    const saved = savedPositions.get(node.id)
    graph.addNode(node.id, {
      x: saved?.x ?? seededCoordinate(node.id, 0),
      y: saved?.y ?? seededCoordinate(node.id, 1),
      size: mapWeightToSize(node.weight ?? 1),
      label: node.label,
      color: colorByGroup(node.group),
      group: node.group ?? 'default',
    })
  })

  graphData.edges.forEach((edge) => {
    if (!graph.hasNode(edge.source) || !graph.hasNode(edge.target) || graph.hasEdge(edge.id)) {
      return
    }
    graph.addEdgeWithKey(edge.id, edge.source, edge.target, {
      size: mapWeightToEdgeSize(edge.weight ?? 1),
      color: GRAPH_THEME.edge,
    })
  })

  const hasSavedLayout = graphData.nodes.some((node) => savedPositions.has(node.id))
  if (!hasSavedLayout) {
    applyGraphLayout(graph, layout, rootNodeIds)
  }

  graph.forEachNode((node, attributes) => {
    savedPositions.set(node, { x: attributes.x, y: attributes.y })
  })

  return graph
}

function seededCoordinate(id: string, salt: number) {
  let hash = salt + 1
  for (let index = 0; index < id.length; index += 1) {
    hash = (hash * 31 + id.charCodeAt(index)) >>> 0
  }
  return (hash % 1000) / 500 - 1
}

function bindNodeDragging(
  renderer: Sigma<SigmaNodeAttrs, SigmaEdgeAttrs>,
  graph: Graph<SigmaNodeAttrs, SigmaEdgeAttrs>,
  nodePositionsRef: { current: Map<string, NodePosition> },
  onNodeSelect?: (nodeId: string) => void,
) {
  let draggedNode: string | null = null
  let dragStart: { x: number; y: number } | null = null
  let moved = false

  const onDownNode = ({ node, event }: { node: string; event: { x: number; y: number } }) => {
    draggedNode = node
    dragStart = { x: event.x, y: event.y }
    moved = false
    renderer.getCamera().disable()
  }

  const onMoveBody = ({ event }: { event: { x: number; y: number; preventSigmaDefault(): void } }) => {
    if (!draggedNode) {
      return
    }

    if (dragStart) {
      const dx = event.x - dragStart.x
      const dy = event.y - dragStart.y
      if (dx * dx + dy * dy > 9) {
        moved = true
      }
    }

    const position = renderer.viewportToGraph(event)
    graph.setNodeAttribute(draggedNode, 'x', position.x)
    graph.setNodeAttribute(draggedNode, 'y', position.y)
    nodePositionsRef.current.set(draggedNode, position)
    event.preventSigmaDefault()
  }

  const releaseDrag = () => {
    draggedNode = null
    dragStart = null
    renderer.getCamera().enable()
  }

  const onClickNode = ({ node }: { node: string }) => {
    if (moved) {
      moved = false
      return
    }
    onNodeSelect?.(node)
  }

  const onClickStage = () => {
    if (moved) {
      moved = false
      return
    }
    onNodeSelect?.('')
  }

  renderer.on('downNode', onDownNode)
  renderer.on('moveBody', onMoveBody)
  renderer.on('upNode', releaseDrag)
  renderer.on('upStage', releaseDrag)
  renderer.on('clickNode', onClickNode)
  renderer.on('clickStage', onClickStage)

  return {
    cleanup: () => {
      renderer.off('downNode', onDownNode)
      renderer.off('moveBody', onMoveBody)
      renderer.off('upNode', releaseDrag)
      renderer.off('upStage', releaseDrag)
      renderer.off('clickNode', onClickNode)
      renderer.off('clickStage', onClickStage)
    },
  }
}

function colorByGroup(group?: string) {
  if (group === 'wallet') {
    return GRAPH_THEME.groups.wallet
  }
  if (group === 'exchange') {
    return GRAPH_THEME.groups.exchange
  }
  if (group === 'risk') {
    return GRAPH_THEME.groups.risk
  }
  return GRAPH_THEME.groups.default
}

function mapWeightToSize(weight: number) {
  return Math.max(4, Math.min(14, 3 + weight * 0.55))
}

function mapWeightToEdgeSize(weight: number) {
  return Math.max(0.35, Math.min(1.4, 0.3 + weight * 0.12))
}

function getNeighborhood(graph: Graph<SigmaNodeAttrs, SigmaEdgeAttrs>, nodeId: string) {
  const neighbors = new Set<string>()
  graph.forEachNeighbor(nodeId, (neighbor: string) => {
    neighbors.add(neighbor)
  })
  return neighbors
}

function isSameAddress(left: string, right: string | null | undefined) {
  if (!right) {
    return false
  }
  return left.toLowerCase() === right.toLowerCase()
}

function isRootAddress(nodeId: string, rootNodeIds: ReadonlySet<string> | null | undefined) {
  if (!rootNodeIds || rootNodeIds.size === 0) {
    return false
  }
  for (const rootNodeId of rootNodeIds) {
    if (isSameAddress(nodeId, rootNodeId)) {
      return true
    }
  }
  return false
}
