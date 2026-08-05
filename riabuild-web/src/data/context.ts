import { createContext, useContext } from "react";
import { Data } from "./types";

export const DataContext = createContext<Data | null>(null);

export function useData(): Data {
  const data = useContext(DataContext);
  if (data === null) {
    throw new Error(
      "useData() was called outside a data provider. Wrap the tree in ConvexDataProvider or DevDataProvider.",
    );
  }
  return data;
}
