/*
 * Adapted from React Bits BlurText (TS-CSS), pinned at
 * 5d26a6709ad7724ea7878e8816dc99facfba9d1a.
 * Copyright (c) 2026 David Haz.
 * Licensed under the MIT + Commons Clause License Condition v1.0; see
 * THIRD_PARTY_NOTICES.md for the source link and complete license notice.
 */

import { motion, type Transition, useReducedMotion } from "motion/react";
import { useEffect, useMemo, useRef, useState } from "react";

type AnimationSnapshot = Record<string, string | number>;

interface BlurTextProps {
  text: string;
  className?: string;
  /** Milliseconds between characters or words, matching the React Bits API. */
  delay?: number;
  animateBy?: "words" | "letters";
  direction?: "top" | "bottom";
  threshold?: number;
  rootMargin?: string;
  animationFrom?: AnimationSnapshot;
  animationTo?: AnimationSnapshot[];
  easing?: (time: number) => number;
  stepDuration?: number;
}

function buildKeyframes(
  from: AnimationSnapshot,
  steps: AnimationSnapshot[],
): Record<string, Array<string | number>> {
  const keys = new Set([...Object.keys(from), ...steps.flatMap((step) => Object.keys(step))]);
  const keyframes: Record<string, Array<string | number>> = {};
  keys.forEach((key) => {
    keyframes[key] = [from[key], ...steps.map((step) => step[key])];
  });
  return keyframes;
}

/**
 * React Bits' sequential blur-to-focus text treatment, adapted to a semantic
 * inline element and reduced-motion behavior suitable for a desktop launcher.
 */
export function BlurText({
  text,
  className,
  delay = 20,
  animateBy = "letters",
  direction = "bottom",
  threshold = 0.1,
  rootMargin = "0px",
  animationFrom,
  animationTo,
  easing = (time) => time,
  stepDuration = 0.18,
}: BlurTextProps) {
  const prefersReducedMotion = useReducedMotion();
  const [inView, setInView] = useState(false);
  const ref = useRef<HTMLSpanElement>(null);
  const elements = useMemo(
    () => (animateBy === "words" ? text.split(" ") : Array.from(text)),
    [animateBy, text],
  );

  useEffect(() => {
    if (prefersReducedMotion) {
      setInView(true);
      return undefined;
    }

    const element = ref.current;
    if (!element || typeof IntersectionObserver === "undefined") {
      setInView(true);
      return undefined;
    }

    const observer = new IntersectionObserver(
      ([entry]) => {
        if (entry?.isIntersecting) {
          setInView(true);
          observer.unobserve(element);
        }
      },
      { threshold, rootMargin },
    );
    observer.observe(element);
    return () => observer.disconnect();
  }, [prefersReducedMotion, rootMargin, threshold]);

  const defaultFrom = useMemo<AnimationSnapshot>(
    () => ({
      filter: "blur(6px)",
      opacity: 0,
      y: direction === "top" ? -8 : 8,
    }),
    [direction],
  );
  const defaultTo = useMemo<AnimationSnapshot[]>(
    () => [
      {
        filter: "blur(2px)",
        opacity: 0.64,
        y: direction === "top" ? 2 : -2,
      },
      { filter: "blur(0px)", opacity: 1, y: 0 },
    ],
    [direction],
  );
  const fromSnapshot = animationFrom ?? defaultFrom;
  const toSnapshots = animationTo ?? defaultTo;
  const keyframes = useMemo(
    () => buildKeyframes(fromSnapshot, toSnapshots),
    [fromSnapshot, toSnapshots],
  );
  const steps = toSnapshots.length + 1;
  const transition: Transition = {
    duration: stepDuration * Math.max(0, steps - 1),
    times: Array.from({ length: steps }, (_, index) => (steps === 1 ? 0 : index / (steps - 1))),
    ease: easing,
  };

  return (
    <span aria-label={text} className={className} ref={ref}>
      {elements.map((segment, index) => (
        <motion.span
          aria-hidden="true"
          initial={prefersReducedMotion ? false : fromSnapshot}
          animate={prefersReducedMotion ? undefined : (inView ? keyframes : fromSnapshot)}
          key={`${segment}-${index}`}
          style={{ display: "inline-block", willChange: "transform, filter, opacity" }}
          transition={prefersReducedMotion
            ? { duration: 0 }
            : { ...transition, delay: (index * delay) / 1000 }}
        >
          {segment === " " ? "\u00a0" : segment}
          {animateBy === "words" && index < elements.length - 1 ? "\u00a0" : null}
        </motion.span>
      ))}
    </span>
  );
}
