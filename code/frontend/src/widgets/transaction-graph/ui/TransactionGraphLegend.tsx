const legendItems = [
  { label: 'Origin', color: '#facc15' },
  { label: 'Wallet', color: '#7eb6ff' },
  { label: 'Exchange', color: '#c4b0f5' },
  { label: 'Risk', color: '#f09494' },
  { label: 'Default', color: '#a1a1aa' },
] as const

export const TransactionGraphLegend = () => {
  return (
    <div className='pointer-events-none absolute bottom-3 left-3 z-10 flex flex-wrap items-center gap-x-2.5 gap-y-0.5 rounded border border-border/30 bg-background/40 px-2 py-1 text-[10px] leading-none text-muted-foreground/70 backdrop-blur-[2px]'>
      {legendItems.map((item) => (
        <span key={item.label} className='inline-flex items-center gap-1'>
          <span className='size-1.5 shrink-0 rounded-full opacity-70' style={{ backgroundColor: item.color }} />
          {item.label}
        </span>
      ))}
    </div>
  )
}
