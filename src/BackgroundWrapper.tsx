import { ReactNode, useState, useEffect } from "react";
import "./App.css";
import images from "./images.json";

interface BackgroundWrapperProps {
  children: ReactNode;
}

/**
 * Helper function to fetch and preload the background image as a blob
 */
async function fetchImage(url: string): Promise<string> {
  const response = await fetch(url);
  if (!response.ok) {
    throw new Error(`Failed to fetch image: ${response.statusText}`);
  }
  const blob = await response.blob();
  return URL.createObjectURL(blob);
}

/**
 * Resolves the best matching width x height category based on viewport size
 */
function getBestMatchingCategory(width: number, height: number): string {
  // Filter for images that are large enough to cover the viewport
  const eligible = images.filter(img => width <= img.width && height <= img.height);
  
  let bestMatch;
  if (eligible.length > 0) {
    // Find the smallest eligible image by area (to avoid loading too-large images)
    const minArea = Math.min(...eligible.map(img => img.width * img.height));
    bestMatch = eligible.find(img => img.width * img.height === minArea);
  } else {
    // Fallback: If viewport is larger than all images, use the largest image by area
    const maxArea = Math.max(...images.map(img => img.width * img.height));
    bestMatch = images.find(img => img.width * img.height === maxArea);
  }
  
  return bestMatch ? `${bestMatch.width}x${bestMatch.height}` : "1920x1080";
}

/**
 * Selects a random image path from the matched category
 */
function getRandomImage(category: string): string {
  const [wStr, hStr] = category.split("x");
  const targetW = Number(wStr);
  const targetH = Number(hStr);

  const list = images.filter(img => img.width === targetW && img.height === targetH);
  if (list.length === 0) return "";
  const randomIndex = Math.floor(Math.random() * list.length);
  return list[randomIndex].url;
}

function BackgroundWrapper({ children }: BackgroundWrapperProps) {
  const [windowWidth, setWindowWidth] = useState(
    typeof window !== "undefined" ? window.innerWidth : 1024
  );
  const [windowHeight, setWindowHeight] = useState(
    typeof window !== "undefined" ? window.innerHeight : 768
  );

  const [activeCategory, setActiveCategory] = useState<string>("");
  const [activeImage, setActiveImage] = useState<string>("");

  // 1. Listen for window resize events to update current width & height
  useEffect(() => {
    const handleResize = () => {
      setWindowWidth(window.innerWidth);
      setWindowHeight(window.innerHeight);
    };
    // Call once on mount to ensure we capture the correct initial width and height
    handleResize();
    
    window.addEventListener("resize", handleResize);
    return () => window.removeEventListener("resize", handleResize);
  }, []);

  // 2. Select, fetch, and update image when width/height category changes
  const currentCategory = getBestMatchingCategory(windowWidth, windowHeight);

  useEffect(() => {
    // Only load a new image if the resolved category changes
    if (currentCategory === activeCategory && activeImage) return;

    const nextRawUrl = getRandomImage(currentCategory);
    if (!nextRawUrl) return;

    let isMounted = true;

    fetchImage(nextRawUrl)
      .then((objectUrl) => {
        if (!isMounted) return;

        setActiveImage((prev) => {
          if (prev && prev.startsWith("blob:")) {
            URL.revokeObjectURL(prev);
          }
          return objectUrl;
        });
        setActiveCategory(currentCategory);
      })
      .catch((error) => {
        console.error("Failed to prefetch image:", error);
        // Fallback directly to the raw URL path if fetch fails
        if (isMounted) {
          setActiveImage(nextRawUrl);
          setActiveCategory(currentCategory);
        }
      });

    return () => {
      isMounted = false;
    };
  }, [currentCategory]);


  // 3. Clean up active blob URLs to prevent memory leaks when wrapper unmounts
  useEffect(() => {
    return () => {
      setActiveImage((img) => {
        if (img && img.startsWith("blob:")) URL.revokeObjectURL(img);
        return "";
      });
    };
  }, []);

  return (
    <div className="relative min-h-screen text-slate-900">
      {/* Master Background Image Layer */}
      {activeImage && (
        <div 
          className="fixed inset-0 -z-20 bg-cover bg-center bg-no-repeat contrast-[1.1] sepia-[0.15]"
          style={{ backgroundImage: `url('${activeImage}')` }} 
        />
      )}

      {/* Master Grain Overlay Layer */}
      <div 
        className="fixed inset-0 -z-10 mix-blend-multiply pointer-events-none" 
        style={{
          filter: `url("data:image/svg+xml;utf8,<svg xmlns='http://www.w3.org/2000/svg'><filter id='noiseFilter'><feTurbulence type='fractalNoise' baseFrequency='0.99' numOctaves='4' result='noise'/><feComponentTransfer in='noise' result='sharpNoise'><feFuncR type='linear' slope='3' intercept='-1'/><feFuncG type='linear' slope='3' intercept='-1'/><feFuncB type='linear' slope='3' intercept='-1'/></feComponentTransfer><feColorMatrix type='matrix' values='0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0.25 0'/></filter></svg>#noiseFilter")`
        }}
      />

      {/* Render whatever is inside the wrapper */}
      {children}
    </div>
  );
}

export default BackgroundWrapper;
