import forceAtlas2 from 'graphology-layout-forceatlas2'
import Graph from 'graphology'

import type { GraphLayoutMode } from '@/shared/graph/contract/graph'

type LayoutNodeAttrs = {
  x: number
  y: number
  size?: number
}

type LayoutGraph = Graph<LayoutNodeAttrs>

export function applyGraphLayout(
  graph: LayoutGraph,
  layout: GraphLayoutMode,
  rootNodeIds: ReadonlySet<string> | null = null,
) {
  if (graph.order === 0) {
    return
  }

  switch (layout) {
    case 'breadthfirst':
      assignBreadthFirst(graph, rootNodeIds)
      break
    case 'spiral':
      assignSpiral(graph, rootNodeIds)
      break
    case 'grid':
      assignGrid(graph)
      break
    case 'force':
      applyForceLayout(graph)
      break
  }
}

function applyForceLayout(graph: LayoutGraph) {
  if (graph.order <= 1) {
    return
  }

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

function assignBreadthFirst(graph: LayoutGraph, rootNodeIds: ReadonlySet<string> | null) {
  const depths = computeUndirectedDepths(graph, pickLayoutRoot(graph, rootNodeIds))
  assignLayeredPositions(graph, depths, { direction: 'vertical', layerGap: 0.35, layerSpread: 1.4 })
}

function assignGrid(graph: LayoutGraph) {
  const nodes = [...graph.nodes()].sort()
  const columns = Math.max(1, Math.ceil(Math.sqrt(nodes.length)))
  const rows = Math.max(1, Math.ceil(nodes.length / columns))

  nodes.forEach((node, index) => {
    const column = index % columns
    const row = Math.floor(index / columns)
    const x = columns === 1 ? 0 : (column / (columns - 1)) * 1.4 - 0.7
    const y = rows === 1 ? 0 : (row / (rows - 1)) * 1.1 - 0.55
    graph.setNodeAttribute(node, 'x', x)
    graph.setNodeAttribute(node, 'y', y)
  })
}

function assignSpiral(graph: LayoutGraph, rootNodeIds: ReadonlySet<string> | null) {
  const root = pickLayoutRoot(graph, rootNodeIds)
  const sorted = [...graph.nodes()].sort((left, right) => graph.degree(right) - graph.degree(left))
  const rootIndex = sorted.indexOf(root)
  if (rootIndex > 0) {
    sorted.splice(rootIndex, 1)
    sorted.unshift(root)
  }

  const goldenAngle = Math.PI * (3 - Math.sqrt(5))
  sorted.forEach((node, index) => {
    if (index === 0) {
      graph.setNodeAttribute(node, 'x', 0)
      graph.setNodeAttribute(node, 'y', 0)
      return
    }
    const radius = 0.12 + Math.sqrt(index) * 0.12
    const angle = index * goldenAngle
    graph.setNodeAttribute(node, 'x', Math.cos(angle) * radius)
    graph.setNodeAttribute(node, 'y', Math.sin(angle) * radius)
  })
}

function pickLayoutRoot(graph: LayoutGraph, rootNodeIds: ReadonlySet<string> | null) {
  const nodes = graph.nodes()
  const fallback = nodes[0]!

  if (rootNodeIds && rootNodeIds.size > 0) {
    let bestRoot = fallback
    let bestDegree = -1

    for (const node of nodes) {
      if (!isRootAddress(node, rootNodeIds)) {
        continue
      }
      const degree = graph.degree(node)
      if (degree > bestDegree) {
        bestRoot = node
        bestDegree = degree
      }
    }

    if (bestDegree >= 0) {
      return bestRoot
    }
  }

  return nodes.reduce((best, current) => (graph.degree(current) > graph.degree(best) ? current : best), fallback)
}

function computeUndirectedDepths(graph: LayoutGraph, root: string) {
  const depths = new Map<string, number>([[root, 0]])
  const queue = [root]

  while (queue.length > 0) {
    const current = queue.shift()!
    const nextDepth = (depths.get(current) ?? 0) + 1

    graph.forEachNeighbor(current, (neighbor) => {
      if (!depths.has(neighbor)) {
        depths.set(neighbor, nextDepth)
        queue.push(neighbor)
      }
    })
  }

  let maxKnownDepth = 0
  depths.forEach((depth) => {
    maxKnownDepth = Math.max(maxKnownDepth, depth)
  })

  graph.forEachNode((node) => {
    if (!depths.has(node)) {
      depths.set(node, maxKnownDepth + 1)
    }
  })

  return depths
}

function groupNodesByDepth(graph: LayoutGraph, depths: Map<string, number>) {
  const layers = new Map<number, string[]>()

  graph.forEachNode((node) => {
    const depth = depths.get(node) ?? 0
    const layer = layers.get(depth) ?? []
    layer.push(node)
    layers.set(depth, layer)
  })

  return layers
}

function assignLayeredPositions(
  graph: LayoutGraph,
  depths: Map<string, number>,
  options: {
    direction: 'vertical' | 'horizontal'
    layerGap: number
    layerSpread: number
  },
) {
  const layers = groupNodesByDepth(graph, depths)
  const maxWidth = Math.max(...Array.from(layers.values()).map((layer) => layer.length), 1)

  for (const [depth, layer] of layers.entries()) {
    const sortedLayer = [...layer].sort()

    sortedLayer.forEach((node, index) => {
      const primary = depth * options.layerGap
      const secondary = maxWidth === 1 ? 0 : (index / (maxWidth - 1)) * options.layerSpread - options.layerSpread / 2

      if (options.direction === 'vertical') {
        graph.setNodeAttribute(node, 'x', secondary)
        graph.setNodeAttribute(node, 'y', primary)
        return
      }

      graph.setNodeAttribute(node, 'x', primary)
      graph.setNodeAttribute(node, 'y', secondary)
    })
  }
}

function isRootAddress(nodeId: string, rootNodeIds: ReadonlySet<string>) {
  for (const rootNodeId of rootNodeIds) {
    if (nodeId.toLowerCase() === rootNodeId.toLowerCase()) {
      return true
    }
  }
  return false
}
