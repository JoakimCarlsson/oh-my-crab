import { notify } from "./events";

/** One line of a cart: a product and how many of it. */
export interface Line {
  sku: string;
  quantity: number;
  price: number;
}

/** A shopper's cart. */
export class Cart {
  private lines: Line[] = [];

  /**
   * @param id The id the cart is saved under.
   */
  constructor(public readonly id: string) {}

  /**
   * Adds a line, or raises the quantity of the line with the same product.
   * @param line The line to add.
   * @returns Nothing.
   */
  addItem(line: Line): void {
    const held = this.lines.find((each) => each.sku === line.sku);
    if (held) {
      held.quantity += line.quantity;
    } else {
      this.lines.push({ ...line });
    }
    notify("cart");
  }

  /**
   * Removes the line of a product.
   * @param sku The product to remove.
   * @returns Whether a line was removed.
   */
  removeItem(sku: string): boolean {
    const before = this.lines.length;
    this.lines = this.lines.filter((each) => each.sku !== sku);
    if (this.lines.length === before) {
      return false;
    }
    notify("cart");
    return true;
  }

  /**
   * Puts back the lines a saved cart held.
   * @param saved The lines to put back.
   * @returns Nothing.
   */
  restore(saved: Line[]): void {
    this.lines = saved.map((each) => ({ ...each }));
    notify("cart");
  }

  /**
   * Empties the cart.
   * @returns Nothing.
   */
  clear(): void {
    if (this.lines.length === 0) {
      return;
    }
    this.lines = [];
    notify("cart");
  }

  /**
   * Adds up the cart.
   * @returns The price of every line times its quantity, summed.
   */
  total(): number {
    return this.lines.reduce((sum, each) => sum + each.price * each.quantity, 0);
  }
}
