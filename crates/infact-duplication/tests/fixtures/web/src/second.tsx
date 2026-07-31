export function total(values: number[]): number {
    // Formatting and comments are not syntax tokens.
    let sum=0;
    for(const value of values){
        sum+=value;
    }
    return sum;
}
