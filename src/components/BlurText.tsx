import { motion, useReducedMotion } from "motion/react";

interface BlurTextProps {
  text: string;
  className?: string;
  delay?: number;
}

/**
 * A local, CSS-first adaptation of the React Bits BlurText interaction.
 * Keeping it in-tree makes the desktop bundle deterministic and easy to tune.
 */
export function BlurText({ text, className, delay = 0 }: BlurTextProps) {
  const prefersReducedMotion = useReducedMotion();

  return (
    <span aria-label={text} className={className}>
      {Array.from(text).map((character, index) => (
        <motion.span
          aria-hidden="true"
          className="blur-text__character"
          initial={prefersReducedMotion ? false : { filter: "blur(9px)", opacity: 0, y: 5 }}
          animate={prefersReducedMotion ? undefined : { filter: "blur(0px)", opacity: 1, y: 0 }}
          transition={{
            delay: delay + index * 0.018,
            duration: 0.42,
            ease: [0.16, 1, 0.3, 1],
          }}
          key={character + index}
        >
          {character === " " ? "\u00a0" : character}
        </motion.span>
      ))}
    </span>
  );
}
