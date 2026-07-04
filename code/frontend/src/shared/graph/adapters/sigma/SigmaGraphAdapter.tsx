import assignCircular from 'graphology-layout/circular'
import forceAtlas2 from 'graphology-layout-forceatlas2'
import Graph from 'graphology'
import Sigma from 'sigma'
import { useEffect, useMemo, useRef, useState } from 'react'

import type { GraphAdapterProps, GraphData, GraphLayoutMode } from '@/shared/graph/contract/graph'

const GRAPH_THEME = {
  canvas: '#09090b',
  edge: '#2a2a30',
  edgeActive: '#52525c',
  label: '#52525c',
  labelActive: '#a1a1aa',
  dimmedNode: '#27272a',
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
  const selectedNodeIdRef = useRef(selectedNodeId)
  const visibleNodeIdsRef = useRef(visibleNodeIds)
  const visibleEdgeIdsRef = useRef(visibleEdgeIds)
  const onNodeSelectRef = useRef(onNodeSelect)
  const onNodeHoverRef = useRef(onNodeHover)
  const layoutRef = useRef(layout)
  const graphSignatureRef = useRef('')
  const [canRender, setCanRender] = useState(false)

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
    () => buildGraph(graph, layout, nodePositionsRef.current),
    [graph, layout],
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

        const activeNodeId = selectedNodeIdRef.current
        if (!activeNodeId) {
          return {
            ...data,
            labelColor: GRAPH_THEME.label,
          }
        }

        if (node === activeNodeId) {
          return {
            ...data,
            size: data.size * 1.15,
            labelColor: GRAPH_THEME.labelActive,
          }
        }

        const neighborSet = getNeighborhood(sigmaGraph, activeNodeId)
        if (neighborSet.has(node)) {
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
  }, [selectedNodeId, visibleNodeIds, visibleEdgeIds])

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
    applyLayout(graph, layout)
    compactGraphPositions(graph, 0.72)
    if (layout === 'force') {
      separateOverlappingNodes(graph, 5, 10)
    } else {
      // Keep concentric/flow shape and only lightly resolve direct collisions.
      separateOverlappingNodes(graph, 0.1, 10)
    }
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

function compactGraphPositions(graph: Graph<SigmaNodeAttrs, SigmaEdgeAttrs>, factor: number) {
  const nodes = graph.nodes()
  if (nodes.length === 0) {
    return
  }

  let centerX = 0
  let centerY = 0

  nodes.forEach((node) => {
    centerX += graph.getNodeAttribute(node, 'x')
    centerY += graph.getNodeAttribute(node, 'y')
  })

  centerX /= nodes.length
  centerY /= nodes.length

  nodes.forEach((node) => {
    const x = graph.getNodeAttribute(node, 'x')
    const y = graph.getNodeAttribute(node, 'y')
    graph.setNodeAttribute(node, 'x', centerX + (x - centerX) * factor)
    graph.setNodeAttribute(node, 'y', centerY + (y - centerY) * factor)
  })
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

function applyLayout(graph: Graph<SigmaNodeAttrs, SigmaEdgeAttrs>, layout: GraphLayoutMode) {
  if (layout === 'concentric') {
    assignCircular(graph, { scale: 0.55 })
    return
  }

  if (layout === 'breadthfirst') {
    assignBreadthFirst(graph)
    return
  }

  if (graph.order > 1) {
    const inferred = forceAtlas2.inferSettings(graph)
    forceAtlas2.assign(graph, {
      iterations: 220,
      settings: {
        ...inferred,
        gravity: Math.max(inferred.gravity ?? 1, 4),
        scalingRatio: Math.max(2, (inferred.scalingRatio ?? 10) * 0.35),
        strongGravityMode: true,
        slowDown: 2,
        adjustSizes: true,
      },
    })
  }
}

function assignBreadthFirst(graph: Graph<SigmaNodeAttrs, SigmaEdgeAttrs>) {
  if (graph.order === 0) {
    return
  }

  const nodes = graph.nodes()
  const root = nodes.reduce((best: string, current: string) => {
    return graph.degree(current) > graph.degree(best) ? current : best
  }, nodes[0]!)

  const depth = new Map<string, number>([[root, 0]])
  const queue = [root]

  while (queue.length > 0) {
    const current = queue.shift()!
    const nextDepth = (depth.get(current) ?? 0) + 1
    graph.forEachOutboundNeighbor(current, (neighbor: string) => {
      if (!depth.has(neighbor)) {
        depth.set(neighbor, nextDepth)
        queue.push(neighbor)
      }
    })
    graph.forEachInboundNeighbor(current, (neighbor: string) => {
      if (!depth.has(neighbor)) {
        depth.set(neighbor, nextDepth)
        queue.push(neighbor)
      }
    })
  }

  const layers = new Map<number, string[]>()
  nodes.forEach((node: string) => {
    const level = depth.get(node) ?? 0
    const layer = layers.get(level) ?? []
    layer.push(node)
    layers.set(level, layer)
  })

  const maxWidth = Math.max(...Array.from(layers.values()).map((layer) => layer.length), 1)
  for (const [level, layer] of layers.entries()) {
    layer.forEach((node, index) => {
      const x = maxWidth === 1 ? 0 : (index / (maxWidth - 1)) * 1.4 - 0.7
      const y = level * 0.35
      graph.setNodeAttribute(node, 'x', x)
      graph.setNodeAttribute(node, 'y', y)
    })
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

function separateOverlappingNodes(graph: Graph<SigmaNodeAttrs, SigmaEdgeAttrs>, minDistance: number, iterations: number) {
  const nodes = graph.nodes()
  if (nodes.length < 2) {
    return
  }

  for (let iteration = 0; iteration < iterations; iteration += 1) {
    let adjusted = false

    for (let index = 0; index < nodes.length; index += 1) {
      const first = nodes[index]!
      let x1 = graph.getNodeAttribute(first, 'x')
      let y1 = graph.getNodeAttribute(first, 'y')

      for (let compareIndex = index + 1; compareIndex < nodes.length; compareIndex += 1) {
        const second = nodes[compareIndex]!
        let x2 = graph.getNodeAttribute(second, 'x')
        let y2 = graph.getNodeAttribute(second, 'y')
        const size1 = graph.getNodeAttribute(first, 'size')
        const size2 = graph.getNodeAttribute(second, 'size')

        const dx = x2 - x1
        const dy = y2 - y1
        const distance = Math.hypot(dx, dy)
        const requiredDistance = Math.max(minDistance, (size1 + size2) * 0.018)

        if (distance >= requiredDistance) {
          continue
        }

        const safeDistance = distance < 1e-4 ? 1e-4 : distance
        const overlap = (requiredDistance - safeDistance) * 0.5
        const shiftX = (dx / safeDistance) * overlap
        const shiftY = (dy / safeDistance) * overlap

        if (distance < 1e-4) {
          const angle = ((index + 1) * (compareIndex + 3) * 0.61) % (Math.PI * 2)
          x1 -= Math.cos(angle) * overlap
          y1 -= Math.sin(angle) * overlap
          x2 += Math.cos(angle) * overlap
          y2 += Math.sin(angle) * overlap
        } else {
          x1 -= shiftX
          y1 -= shiftY
          x2 += shiftX
          y2 += shiftY
        }

        graph.setNodeAttribute(first, 'x', x1)
        graph.setNodeAttribute(first, 'y', y1)
        graph.setNodeAttribute(second, 'x', x2)
        graph.setNodeAttribute(second, 'y', y2)
        adjusted = true
      }
    }

    if (!adjusted) {
      break
    }
  }
}

function getNeighborhood(graph: Graph<SigmaNodeAttrs, SigmaEdgeAttrs>, nodeId: string) {
  const neighbors = new Set<string>()
  graph.forEachNeighbor(nodeId, (neighbor: string) => {
    neighbors.add(neighbor)
  })
  return neighbors
}
