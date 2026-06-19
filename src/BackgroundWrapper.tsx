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
/**
 * Resolves the list of eligible images based on viewport size
 */
function getEligibleImages(width: number, height: number) {
  // Filter for images that are large enough to cover the viewport
  const eligible = images.filter(img => width <= img.width && height <= img.height);
  
  if (eligible.length > 0) {
    return eligible;
  } else {
    // Fallback: If viewport is larger than all images, use the largest image(s) by area
    const maxArea = Math.max(...images.map(img => img.width * img.height));
    return images.filter(img => img.width * img.height === maxArea);
  }
}

/**
 * Selects a random image path from the comma-separated eligible image URLs
 */
function getRandomImageFromKeys(keysStr: string): string {
  const list = keysStr.split(",");
  if (list.length === 0 || !list[0]) return "";
  const randomIndex = Math.floor(Math.random() * list.length);
  return list[randomIndex];
}

function BackgroundWrapper({ children }: BackgroundWrapperProps) {
  const [windowWidth, setWindowWidth] = useState(
    typeof window !== "undefined" ? window.innerWidth : 1024
  );
  const [windowHeight, setWindowHeight] = useState(
    typeof window !== "undefined" ? window.innerHeight : 768
  );

  const [activeImage, setActiveImage] = useState<string>("");
  const [activeImageUrl, setActiveImageUrl] = useState<string>("");

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

  // 2. Select, fetch, and update image when eligible image pool changes
  const eligibleImages = getEligibleImages(windowWidth, windowHeight);
  const eligibleKeys = eligibleImages.map(img => img.url).sort().join(",");

  useEffect(() => {
    const nextRawUrl = getRandomImageFromKeys(eligibleKeys);
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
        setActiveImageUrl(nextRawUrl);
      })
      .catch((error) => {
        console.error("Failed to prefetch image:", error);
        // Fallback directly to the raw URL path if fetch fails
        if (isMounted) {
          setActiveImage(nextRawUrl);
          setActiveImageUrl(nextRawUrl);
        }
      });

    return () => {
      isMounted = false;
    };
  }, [eligibleKeys]);


  // 3. Clean up active blob URLs to prevent memory leaks when wrapper unmounts
  useEffect(() => {
    return () => {
      setActiveImage((img) => {
        if (img && img.startsWith("blob:")) URL.revokeObjectURL(img);
        return "";
      });
    };
  }, []);

  const activeImgObj = images.find(img => img.url === activeImageUrl);
  const opacity = activeImgObj && "opacity" in activeImgObj ? activeImgObj.opacity : 1.0;

  return (
    <div className="relative min-h-screen text-slate-200">
      {/* Master Background Color Layer */}
      <div className="fixed inset-0 z-0 bg-black" />

      {/* Master Background Image Layer */}
      {activeImage && (
        <div 
          className="fixed inset-0 z-0 bg-cover bg-center bg-no-repeat contrast-[1.1] sepia-[0.15]"
          style={{ backgroundImage: `url('${activeImage}')`, opacity }} 
        />
      )}

      {/* Master Grain Overlay Layer */}
      <div 
        className="fixed inset-0 z-0 mix-blend-multiply pointer-events-none" 
        style={{
          filter: `url("data:image/svg+xml;utf8,<svg xmlns='http://www.w3.org/2000/svg'><filter id='noiseFilter'><feTurbulence type='fractalNoise' baseFrequency='0.49' numOctaves='5' result='noise'/><feComponentTransfer in='noise' result='sharpNoise'><feFuncR type='linear' slope='3' intercept='-1'/><feFuncG type='linear' slope='3' intercept='-1'/><feFuncB type='linear' slope='3' intercept='-1'/></feComponentTransfer><feColorMatrix type='matrix' values='0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0.25 0'/></filter></svg>#noiseFilter")`
        }}
      />

      {/* Render whatever is inside the wrapper */}
      <div className="relative z-10 min-h-screen">
        {children}
      </div>
    </div>
  );
}

export default BackgroundWrapper;

