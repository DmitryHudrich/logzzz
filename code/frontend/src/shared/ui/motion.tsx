import { AnimatePresence, motion, type HTMLMotionProps } from 'framer-motion'

type FadeInProps = HTMLMotionProps<'div'> & {
  show?: boolean
}

export const FadeIn = ({ show = true, children, ...props }: FadeInProps) => {
  return (
    <AnimatePresence>
      {show ? (
        <motion.div initial={{ opacity: 0, y: 8 }} animate={{ opacity: 1, y: 0 }} exit={{ opacity: 0, y: 8 }} {...props}>
          {children}
        </motion.div>
      ) : null}
    </AnimatePresence>
  )
}

export const motionContainer = {
  hidden: { opacity: 0 },
  show: {
    opacity: 1,
    transition: { staggerChildren: 0.05 },
  },
}

export const motionItem = {
  hidden: { opacity: 0, y: 6 },
  show: { opacity: 1, y: 0 },
}
